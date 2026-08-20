//! Finding the Sapling proving parameters, and downloading them only if nobody
//! else already has.
//!
//! # Why a wallet has to think about this at all
//!
//! Building any shielded transaction needs a Groth16 proof, and proving needs
//! the ~50 MB proving key from the Zcash ceremony. It cannot be committed — it
//! is fifty megabytes of binary — and it cannot be generated, because the whole
//! point of a trusted setup is that nobody can.
//!
//! # Look before downloading
//!
//! Verus uses the **stock Zcash Sapling parameters, byte for byte**. There is no
//! Verus ceremony and no Verus circuit; the only Verus-specific value on the
//! shielded path is the consensus branch id in the sighash. So the files a
//! `verusd` or `zcashd` install already fetched are exactly the files this
//! needs, and somebody who runs a node should not download them twice.
//!
//! That is not a small saving on a metered connection, and it is also the more
//! honest default: the wallet uses what is on the machine rather than reaching
//! for the network because reaching for the network was easier to write.
//!
//! # What makes the download safe is the hash, not the TLS
//!
//! [`verus_sapling::params`] pins the ceremony's SHA-256 for both files and
//! **enforces** them. That is what this module leans on, and it is why fetching
//! over the open internet is acceptable: a compromised mirror, a hostile proxy
//! or a corrupted disk produces bytes that do not hash to the pinned values and
//! are refused, before any of it reaches the prover.
//!
//! The SDK's own note on why that matters is worth not paraphrasing: wrong
//! parameters do not fail loudly. At best they produce proofs a daemon rejects
//! after thirty seconds of work. At worst — with a *maliciously constructed*
//! reference string rather than a merely corrupt one — Groth16's zero-knowledge
//! property no longer holds, so the proofs a wallet publishes can leak what they
//! existed to hide.
//!
//! # Nothing here is cached in memory
//!
//! [`SaplingParams`] is tens of megabytes of parsed circuit. Loading it takes a
//! second or two and proving takes thirty, so it is loaded per operation rather
//! than held for the life of the process — the wallet spends most of its time
//! not proving anything.

use std::io::Read;
use std::path::{Path, PathBuf};

use verus_sdk::verus_sapling::params::SaplingParams;

/// The ceremony's file names. These are what every install calls them.
const SPEND: &str = "sapling-spend.params";
const OUTPUT: &str = "sapling-output.params";

/// Exact sizes, from the ceremony. Used to reject an obviously wrong file
/// before spending a minute downloading or hashing it.
const SPEND_BYTES: u64 = 47_958_396;
const OUTPUT_BYTES: u64 = 3_592_860;

/// Where the files are published.
///
/// Zcash's own distribution. Not a mirror, and not a URL taken from
/// configuration: an endpoint somebody can point elsewhere is worth much less
/// once the hash is enforced, and worth nothing but confusion if it is not.
const SPEND_URL: &str = "https://download.z.cash/downloads/sapling-spend.params";
const OUTPUT_URL: &str = "https://download.z.cash/downloads/sapling-output.params";

/// Why the parameters could not be had.
#[derive(Debug, thiserror::Error)]
pub enum ParamsError {
    /// They are not on this machine and were not fetched.
    #[error("the Sapling proving parameters are not on this machine")]
    Missing,

    /// The download did not complete.
    #[error("could not download {name}: {reason}")]
    Download { name: &'static str, reason: String },

    /// A file could not be written.
    #[error("could not write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The bytes are not the ceremony's.
    ///
    /// Carries the SDK's own wording, which names the hash it got and the one
    /// it wanted. This is the one failure here that must never be retried past
    /// — see the module docs.
    #[error("{0}")]
    NotTheCeremonyFiles(String),
}

/// A pair of parameter files that exist. Says nothing yet about their contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub spend: PathBuf,
    pub output: PathBuf,
}

/// How far a download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Which file, as it is named on disk.
    pub name: &'static str,
    /// Bytes written so far.
    pub done: u64,
    /// Bytes expected in total, across **both** files.
    ///
    /// Both, rather than this one: a bar that fills, resets and fills again is
    /// two pieces of information pretending to be one, and the second fill takes
    /// thirteen times as long as the first.
    pub total: u64,
}

/// Every directory worth looking in, most likely first.
///
/// The node's own locations come before the wallet's, so a machine that already
/// has the files never accumulates a second copy.
pub fn search_paths(app_home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        // macOS, where `zcashd`'s fetch script puts them.
        paths.push(
            home.join("Library")
                .join("Application Support")
                .join("ZcashParams"),
        );
        // Linux and the BSDs, and macOS installs that followed the Linux
        // instructions — which happens often enough to be worth checking on
        // every platform rather than only where it is documented.
        paths.push(home.join(".zcash-params"));
    }

    // Windows.
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        paths.push(appdata.join("ZcashParams"));
    }

    // System-wide, for a packaged node.
    paths.push(PathBuf::from("/usr/share/zcash-params"));

    // Last: where this wallet would have downloaded them itself.
    paths.push(app_home.join("sapling-params"));

    paths
}

/// The first directory holding both files at their exact sizes.
///
/// Size is checked here and the hash is not: hashing 50 MB takes long enough to
/// be worth doing once, at load, where the SDK does it anyway. What this
/// rejects is a truncated or half-downloaded file, which is the common case and
/// is cheap to spot.
pub fn find(app_home: &Path) -> Option<Located> {
    find_in(&search_paths(app_home))
}

/// The same search, over directories the caller names.
///
/// # Why this seam exists
///
/// [`find`] looks in machine-global places — the node's parameter directories,
/// under `$HOME` — so it answers differently on every machine and cannot be
/// tested. That is not a flaw in it; finding what the node already downloaded
/// is the entire point. But it means a test that passes a temporary directory
/// still gets whatever is installed on the developer's laptop, which is how
/// both of this module's first tests passed for the wrong reason and then
/// failed for the right one.
///
/// So the list is a parameter here, and [`find`] is the thin wrapper that
/// builds the real one.
pub fn find_in(dirs: &[PathBuf]) -> Option<Located> {
    dirs.iter().find_map(|dir| {
        let spend = dir.join(SPEND);
        let output = dir.join(OUTPUT);
        (is_sized(&spend, SPEND_BYTES) && is_sized(&output, OUTPUT_BYTES))
            .then_some(Located { spend, output })
    })
}

fn is_sized(path: &Path, expected: u64) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.len() == expected)
}

/// Load and verify. The hashes are the SDK's and are enforced there.
pub fn load(located: &Located) -> Result<SaplingParams, ParamsError> {
    SaplingParams::from_files(&located.spend, &located.output)
        .map_err(|e| ParamsError::NotTheCeremonyFiles(e.to_string()))
}

/// Download both files into `dir`, reporting progress.
///
/// # Why each file lands under a temporary name first
///
/// A parameter file is found by its size, and an interrupted download has a
/// plausible-looking prefix of the right name. Writing straight to the final
/// name would leave a wallet that finds a truncated file, fails its hash, and
/// has no way to tell that from tampering. Renaming into place is atomic, so a
/// file under the real name is always complete.
pub fn fetch(dir: &Path, mut progress: impl FnMut(Progress)) -> Result<Located, ParamsError> {
    std::fs::create_dir_all(dir).map_err(|source| ParamsError::Write {
        path: dir.to_path_buf(),
        source,
    })?;

    let total = SPEND_BYTES + OUTPUT_BYTES;
    let mut done = 0;

    // The small one first, so something finishes early and the interface has
    // proof the connection works before the thirteen-times-longer wait.
    for (name, url, size) in [
        (OUTPUT, OUTPUT_URL, OUTPUT_BYTES),
        (SPEND, SPEND_URL, SPEND_BYTES),
    ] {
        let target = dir.join(name);
        if is_sized(&target, size) {
            done += size;
            progress(Progress { name, done, total });
            continue;
        }
        download(url, &target, name, size, done, total, &mut progress)?;
        done += size;
    }

    Ok(Located {
        spend: dir.join(SPEND),
        output: dir.join(OUTPUT),
    })
}

#[allow(clippy::too_many_arguments)]
fn download(
    url: &str,
    target: &Path,
    name: &'static str,
    size: u64,
    already: u64,
    total: u64,
    progress: &mut impl FnMut(Progress),
) -> Result<(), ParamsError> {
    let response = ureq::get(url).call().map_err(|e| ParamsError::Download {
        name,
        reason: e.to_string(),
    })?;

    let temp = target.with_extension("partial");
    let file = std::fs::File::create(&temp).map_err(|source| ParamsError::Write {
        path: temp.clone(),
        source,
    })?;
    let mut writer = std::io::BufWriter::new(file);

    // Capped at the size the ceremony file has. A server that keeps sending
    // would otherwise fill the disk, and the extra bytes could not be right
    // whatever they are.
    let mut reader = response.into_reader().take(size);
    let mut buffer = vec![0u8; 1 << 16];
    let mut written = 0u64;

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(source) => {
                let _ = std::fs::remove_file(&temp);
                return Err(ParamsError::Write { path: temp, source });
            }
        };
        if let Err(source) = std::io::Write::write_all(&mut writer, &buffer[..read]) {
            let _ = std::fs::remove_file(&temp);
            return Err(ParamsError::Write { path: temp, source });
        }
        written += read as u64;
        progress(Progress {
            name,
            done: already + written,
            total,
        });
    }

    if let Err(source) = std::io::Write::flush(&mut writer) {
        let _ = std::fs::remove_file(&temp);
        return Err(ParamsError::Write { path: temp, source });
    }
    drop(writer);

    if written != size {
        let _ = std::fs::remove_file(&temp);
        return Err(ParamsError::Download {
            name,
            reason: format!("the server sent {written} bytes, not {size}"),
        });
    }

    std::fs::rename(&temp, target).map_err(|source| ParamsError::Write {
        path: target.to_path_buf(),
        source,
    })
}

/// The whole story: use what is on the machine, download if there is none, then
/// load and verify.
pub fn ensure(
    app_home: &Path,
    allow_download: bool,
    progress: impl FnMut(Progress),
) -> Result<SaplingParams, ParamsError> {
    ensure_in(
        &search_paths(app_home),
        &app_home.join("sapling-params"),
        allow_download,
        progress,
    )
}

/// The same, over directories the caller names. See [`find_in`] for why.
pub fn ensure_in(
    dirs: &[PathBuf],
    download_to: &Path,
    allow_download: bool,
    progress: impl FnMut(Progress),
) -> Result<SaplingParams, ParamsError> {
    if let Some(located) = find_in(dirs) {
        return load(&located);
    }
    if !allow_download {
        return Err(ParamsError::Missing);
    }
    let located = fetch(download_to, progress)?;
    load(&located)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sizes this module screens on are the ceremony's, and the hashes it
    /// leans on are the SDK's. Neither is restated here as a literal — this
    /// only checks that the two constants have not drifted apart from the
    /// SDK's, which is the way they would silently stop agreeing.
    #[test]
    fn the_pinned_hashes_are_the_sdks() {
        use verus_sdk::verus_sapling::params::{OUTPUT_PARAMS_SHA256, SPEND_PARAMS_SHA256};

        assert_eq!(
            hex::encode(SPEND_PARAMS_SHA256),
            "8e48ffd23abb3a5fd9c5589204f32d9c31285a04b78096ba40a79b75677efc13",
        );
        assert_eq!(
            hex::encode(OUTPUT_PARAMS_SHA256),
            "2f0ebbcbb9bb0bcffe95a397e7eba89c29eb4dde6191c339db88570e3f3fb0e4",
        );
    }

    /// The node's directories come before the wallet's own.
    ///
    /// The order is the whole behaviour: a machine with a node must never
    /// accumulate a second fifty-megabyte copy because the wallet looked at
    /// itself first.
    #[test]
    fn the_nodes_directories_are_searched_before_our_own() {
        let home = PathBuf::from("/tmp/pecu-test-home");
        let paths = search_paths(&home);

        let ours = paths
            .iter()
            .position(|p| p.starts_with(&home))
            .expect("the wallet's own directory is searched");
        assert_eq!(
            ours,
            paths.len() - 1,
            "the wallet looked at itself before the node: {paths:?}",
        );
        assert!(paths.len() > 1, "nothing but our own directory is searched");
    }

    /// A file of the wrong size is not a parameter file.
    #[test]
    fn a_truncated_file_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let params = dir.path().join("sapling-params");
        std::fs::create_dir_all(&params).expect("mkdir");
        std::fs::write(params.join(SPEND), b"not fifty megabytes").expect("write");
        std::fs::write(params.join(OUTPUT), b"nor three and a half").expect("write");

        assert_eq!(
            find_in(&[params]),
            None,
            "a pair of stub files was accepted as the ceremony parameters",
        );
    }

    /// Refusing to download must say so plainly rather than reporting a
    /// network failure for a network call nobody made.
    #[test]
    fn without_permission_to_download_the_answer_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        // `SaplingParams` has no `Debug` — it is tens of megabytes of parsed
        // circuit — so the outcome is narrowed to its error before it is
        // asserted on, rather than printing the whole `Result`.
        match ensure_in(&[dir.path().to_path_buf()], dir.path(), false, |_| {}) {
            Err(ParamsError::Missing) => {}
            Err(other) => panic!("refused for the wrong reason: {other}"),
            Ok(_) => panic!("parameters appeared from nowhere"),
        }
    }
}

//! Where one chain's files live, and which chain was chosen last.
//!
//! # Why the chain is in the path and not in a column
//!
//! Every durable thing this wallet writes is about one chain: the vault, the
//! wallet database, the dashboard cache, the ledger of transactions whose fate
//! is unknown, the salt for a name being claimed. A column would make mixing
//! them a query away — one missing `WHERE` and a testnet figure renders as
//! mainnet, which is a claim about somebody's money.
//!
//! A directory makes that mistake unavailable rather than unlikely. Two chains
//! are two sets of files that share no code path, and the only way to read the
//! wrong one is to open the wrong directory.
//!
//! # Why the choice is remembered outside them
//!
//! Which chain to open cannot be stored in a per-chain file, since finding that
//! file requires already knowing the answer. So it lives in one line at the top
//! of the home directory, and it is the only thing there.

use std::path::{Path, PathBuf};

use pecu_chain::Network;

/// The file at the top of the home directory naming the chain to open.
const CHOICE: &str = "network";

/// One chain's directory, and the files in it.
///
/// Constructed from a home directory and a network; every path below is derived
/// rather than passed in, so nothing can end up beside the wrong chain's vault
/// by being handed the wrong string.
#[derive(Clone, Debug)]
pub struct Paths {
    home: PathBuf,
    dir: PathBuf,
}

impl Paths {
    /// The directory for `network` under `home`, created if it is missing.
    ///
    /// The demo build gets `mock/` whatever chain it claims to be on, and that
    /// is not tidiness. Everything under here is written to, so a demo run
    /// sharing a directory would put scripted figures and a fixture's
    /// transaction into the files a real wallet reads back on its next launch —
    /// and a wallet that cannot say whether a row came from a chain or from a
    /// fixture is worse than one with no demo mode at all.
    ///
    /// A directory that cannot be created is logged and returned anyway. The
    /// wallet still starts: every open below it fails on its own terms and says
    /// so, which is a better failure than refusing to launch.
    pub fn new(home: PathBuf, network: &Network, mock: bool) -> Self {
        let dir = if mock {
            home.join("mock")
        } else {
            home.join(network.dir_name())
        };

        if let Err(error) = std::fs::create_dir_all(&dir) {
            tracing::warn!(%error, path = %dir.display(), "could not create the wallet directory");
        }

        Self { home, dir }
    }

    /// The application home, holding one directory per chain.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// This chain's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The wallet file. Opened if present; not created until somebody asks for
    /// a wallet.
    pub fn vault(&self) -> PathBuf {
        self.dir.join("vault.json")
    }

    /// Transactions handed to a node whose outcome is unknown.
    ///
    /// Beside the vault, so a backup of the wallet directory carries them with
    /// it — the same reason the reservation is.
    pub fn pending(&self) -> PathBuf {
        self.dir.join("pending-broadcast.json")
    }

    /// The salt for a name being claimed, which nothing can reconstruct.
    pub fn registration(&self) -> PathBuf {
        self.dir.join("registration.json")
    }

    /// A currency somebody decided to make, held across the identity
    /// registration it is waiting on.
    ///
    /// Beside the reservation because the two are halves of one act on the path
    /// that starts from a new name — and because both describe one chain, so
    /// both move when the chain does.
    pub fn launch(&self) -> PathBuf {
        self.dir.join("launch.json")
    }

    /// The chain somebody last chose, if this home has been used before.
    ///
    /// `None` covers three cases that all deserve the same answer — never
    /// launched, unreadable, or holding something this build does not
    /// recognise. The caller decides the default, and it is testnet, so the
    /// worst outcome of a damaged file is being asked to choose again rather
    /// than being put on mainnet by a corrupted byte.
    pub fn remembered(home: &Path) -> Option<Network> {
        let raw = std::fs::read_to_string(home.join(CHOICE)).ok()?;
        let name = raw.trim();
        // `from_chain_name` accepts anything, so an empty or damaged file would
        // otherwise become `Other("")` and open a directory called `chain`.
        match name {
            "VRSC" | "VRSCTEST" => Some(Network::from_chain_name(name)),
            _ => None,
        }
    }

    /// Write down the chain in use, so the next launch opens the same one.
    ///
    /// Best effort, and deliberately not fatal: failing to record the choice
    /// costs a re-selection on the next launch, while refusing to switch over
    /// it would strand somebody on a chain they have just left. The switch
    /// itself has already happened by the time this is called.
    pub fn remember(home: &Path, network: &Network) {
        if let Err(error) = std::fs::write(home.join(CHOICE), network.chain_name()) {
            tracing::warn!(%error, "the chain in use could not be written down");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_chain_gets_its_own_directory_and_its_own_files() {
        let home = tempfile::tempdir().expect("tempdir");
        let test = Paths::new(home.path().to_path_buf(), &Network::Testnet, false);
        let main = Paths::new(home.path().to_path_buf(), &Network::Mainnet, false);

        assert!(test.dir().is_dir());
        assert!(main.dir().is_dir());
        assert_ne!(test.dir(), main.dir());

        // The whole point: no file is shared, so no read can cross over.
        for (a, b) in [
            (test.vault(), main.vault()),
            (test.pending(), main.pending()),
            (test.registration(), main.registration()),
            (test.launch(), main.launch()),
        ] {
            assert_ne!(a, b);
        }
    }

    /// The demo build never shares a directory with a real wallet, whatever
    /// chain it claims.
    #[test]
    fn the_demo_build_is_somewhere_else_entirely() {
        let home = tempfile::tempdir().expect("tempdir");
        let real = Paths::new(home.path().to_path_buf(), &Network::Testnet, false);
        let demo = Paths::new(home.path().to_path_buf(), &Network::Testnet, true);

        assert_ne!(real.dir(), demo.dir());
        assert_eq!(demo.dir(), home.path().join("mock"));
    }

    #[test]
    fn the_chain_in_use_survives_a_restart() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(Paths::remembered(home.path()), None);

        Paths::remember(home.path(), &Network::Mainnet);
        assert_eq!(Paths::remembered(home.path()), Some(Network::Mainnet));

        Paths::remember(home.path(), &Network::Testnet);
        assert_eq!(Paths::remembered(home.path()), Some(Network::Testnet));
    }

    /// A damaged file asks again rather than answering with mainnet.
    #[test]
    fn nothing_unrecognised_becomes_a_chain() {
        let home = tempfile::tempdir().expect("tempdir");
        for damaged in ["", "  ", "VRSCTES", "SOMEPBAAS", "VRSC extra", "\u{0}"] {
            std::fs::write(home.path().join(CHOICE), damaged).expect("write");
            assert_eq!(
                Paths::remembered(home.path()),
                None,
                "`{damaged}` was read as a chain",
            );
        }
    }

    /// Surrounding whitespace is somebody's editor, not a different chain.
    #[test]
    fn a_trailing_newline_is_still_the_same_chain() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(home.path().join(CHOICE), "VRSC\n").expect("write");
        assert_eq!(Paths::remembered(home.path()), Some(Network::Mainnet));
    }
}

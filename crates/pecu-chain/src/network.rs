//! Which chain we are on — as reported, never as assumed.

/// A Verus chain.
///
/// # This is derived from what the node says, never from the URL
///
/// A URL containing "test" proves nothing: an endpoint can be renamed,
/// misconfigured, or proxied. The only trustworthy statement about which chain
/// you are talking to is `ChainInfo::name`, which the daemon reports about
/// itself — so [`Network::from_chain_name`] is the only constructor that
/// matters, and nothing in this crate parses a hostname.
///
/// Getting this wrong is not cosmetic. A wallet that believes it is on testnet
/// while pointed at mainnet would sign a real transaction believing it was
/// worthless.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Testnet,
    /// A PBaaS chain, or anything else this build does not have a name for.
    /// Deliberately carried rather than collapsed into an error: reading a
    /// balance on an unknown chain is fine, and only *spending* needs the
    /// distinction.
    Other(String),
}

impl Network {
    /// Read a network from `ChainInfo::name`.
    pub fn from_chain_name(name: &str) -> Self {
        match name {
            "VRSC" => Self::Mainnet,
            "VRSCTEST" => Self::Testnet,
            other => Self::Other(other.to_string()),
        }
    }

    /// The chain's name as the daemon spells it — the inverse of
    /// [`Network::from_chain_name`].
    ///
    /// Not [`Network::label`], which is for people: VDXF key derivation hashes
    /// this string, so "Testnet" and "VRSCTEST" derive different keys and only
    /// one of them matches anything on chain.
    pub fn chain_name(&self) -> &str {
        match self {
            Self::Mainnet => "VRSC",
            Self::Testnet => "VRSCTEST",
            Self::Other(name) => name,
        }
    }

    /// The directory this chain's files live in, under the application home.
    ///
    /// **Not derived from [`Network::chain_name`].** These two names are pinned
    /// to different things: the chain name is consensus, and changing it would
    /// break VDXF derivation, while this one is a path somebody's wallet is
    /// already sitting in. `testnet/` is what shipped, so `testnet/` is what
    /// this returns — deriving it from "VRSCTEST" would rename the directory
    /// out from under every existing installation and present them with an
    /// empty wallet.
    ///
    /// Lowercased and stripped for [`Network::Other`], because a PBaaS chain
    /// name is not otherwise guaranteed to be a usable path segment.
    pub fn dir_name(&self) -> String {
        match self {
            Self::Mainnet => "mainnet".to_string(),
            Self::Testnet => "testnet".to_string(),
            Self::Other(name) => {
                let cleaned: String = name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_lowercase()
                        } else {
                            '-'
                        }
                    })
                    .collect();
                // A name that cleans down to nothing would collide with the
                // home directory itself and put a PBaaS wallet where the
                // network choice is kept.
                if cleaned.trim_matches('-').is_empty() {
                    "chain".to_string()
                } else {
                    cleaned
                }
            }
        }
    }

    /// Whether this is the chain where mistakes cost real money.
    pub fn is_mainnet(&self) -> bool {
        matches!(self, Self::Mainnet)
    }

    /// How to write it in a UI.
    pub fn label(&self) -> &str {
        match self {
            Self::Mainnet => "Mainnet",
            Self::Testnet => "Testnet",
            Self::Other(name) => name,
        }
    }

    /// What to write next to an amount.
    ///
    /// The chain's own name, which for a Verus chain *is* the ticker — a PBaaS
    /// chain's currency is named after the chain. Testnet coins say `VRSCTEST`
    /// rather than `VRSC` on purpose: a screen that labels them the same is a
    /// screen that has stopped being able to tell you which money you are
    /// looking at.
    pub fn ticker(&self) -> &str {
        match self {
            Self::Mainnet => "VRSC",
            Self::Testnet => "VRSCTEST",
            Self::Other(name) => name,
        }
    }
}

impl Network {
    /// Where to look a transaction up, for the chains that have a public
    /// explorer.
    ///
    /// `None` for anything else — a PBaaS chain has no explorer this build
    /// knows about, and guessing a hostname would send someone to a page that
    /// does not exist, or worse, to somebody else's chain.
    pub fn explorer(&self, txid: &str) -> Option<String> {
        match self {
            // Chainvue's own, which is the explorer this project ships beside.
            //
            // **Testnet only for now.** The mainnet path under the same host is
            // not confirmed, and a guessed one is worse than a different
            // explorer that works: it sends somebody looking for their payment
            // to a page that is not there. `explorer.verus.io` stays until the
            // mainnet form is checked against the live site.
            Self::Mainnet => Some(format!("https://explorer.verus.io/tx/{txid}")),
            Self::Testnet => Some(format!("https://markets.chainvue.io/testnet/tx/{txid}/")),
            Self::Other(_) => None,
        }
    }
}

impl Network {
    /// The chains this build ships, in the order they are offered.
    ///
    /// Testnet first, deliberately: it is the default for a wallet that has
    /// never been launched, and the one where a mistake costs nothing.
    ///
    /// The three after mainnet are PBaaS chains, each its own chain with its
    /// own coins, its own history and its own node — not endpoints for VRSC.
    /// They are `Other`, which is the variant that has always carried a chain
    /// this enum does not name, and every guard that matters keys on
    /// [`Network::is_mainnet`] rather than on the variant.
    pub fn shipped() -> Vec<Self> {
        vec![
            Self::Testnet,
            Self::Mainnet,
            Self::Other("VARRR".to_string()),
            Self::Other("CHIPS".to_string()),
            Self::Other("VDEX".to_string()),
        ]
    }

    /// What this chain is called on a button.
    ///
    /// [`Network::label`] answers with the chain's own name — `VRSCTEST`,
    /// `VARRR` — which is what a node reports and what has to be compared
    /// against. This is what a person recognises.
    pub fn title(&self) -> &str {
        match self {
            Self::Mainnet => "Verus",
            Self::Testnet => "Testnet",
            Self::Other(name) => match name.as_str() {
                "VARRR" | "vARRR" => "Pirate Chain",
                "CHIPS" => "CHIPS",
                "VDEX" | "vDEX" => "vDEX",
                other => other,
            },
        }
    }

    /// The endpoints this build ships for this chain.
    ///
    /// **A function of the chain**, which is the whole point. It used to be one
    /// list in `pecu-app`, so a wallet on VRSC was offered `api.verustest.net`
    /// and marked it `WrongNetwork` the moment it answered — correct, and
    /// untidy, and it put an endpoint on screen that could never be used.
    ///
    /// A chain this build has no endpoint for gets an empty list, which the
    /// node screen already has an honest empty state for.
    pub fn builtin_nodes(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Mainnet => &[("VRSC (public)", "https://api.verus.services")],
            Self::Testnet => &[("VRSCTEST (public)", "https://api.verustest.net")],
            Self::Other(name) => match name.as_str() {
                "VARRR" | "vARRR" => &[("vARRR (public)", "https://vapi.piratechain.com/")],
                "CHIPS" => &[("CHIPS (public)", "https://api.chips.cash/")],
                "VDEX" | "vDEX" => &[("vDEX (public)", "https://api.vdex.to/")],
                _ => &[],
            },
        }
    }
}

/// Where a chain publishes whether the protocol has switched anything off.
///
/// A [notification oracle], which for every chain here is the chain's own root
/// identity. Its `contentmultimap` carries at most one upgrade descriptor,
/// under a VDXF key derived per chain — and the key **must not** be reused
/// across chains, which is why this is a pair and not a constant.
///
/// [notification oracle]: https://docs.verus.io
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Oracle {
    /// The identity to ask for.
    pub identity: &'static str,
    /// The one key in its content map worth reading. Other entries appear —
    /// testnet's oracle carries one — and they are different record types, not
    /// upgrade descriptors.
    pub content_key: &'static str,
}

impl Network {
    /// The oracle for this chain, or `None` when there is not one.
    ///
    /// `None` is **not** "nothing is switched off". It is "there is nowhere to
    /// ask", which is a third state the caller has to keep apart from a clear
    /// answer and from a failed one — see `pecu_core::upgrade`.
    /// The grpc-web endpoint this chain's shielded notes are read from.
    ///
    /// A **different server from the RPC node**, speaking a different protocol
    /// for a different purpose: the node answers about transparent addresses,
    /// and this streams compact blocks so a wallet can trial-decrypt them
    /// without telling anyone which notes are its own.
    ///
    /// # Why this is lightwalletd's own address
    ///
    /// It was not always. `verus-light` speaks grpc-web over HTTP/1.1 and
    /// lightwalletd speaks native gRPC over HTTP/2, so for a while this had to
    /// name a translating proxy — which meant naming *somebody's* proxy, and
    /// routing every user's block requests through whoever ran it. What was
    /// shipped here was chainvue's own, which is a poor default for a privacy
    /// feature: it concentrated the one thing a light client leaks onto a box
    /// belonging to the people who wrote the wallet.
    ///
    /// [`crate::grpc::GrpcTransport`] removed the need. This is now Verus'
    /// public testnet lightwalletd, reached directly, with nobody in between —
    /// and a private proxy remains perfectly usable, because
    /// [`crate::LightServer::connect`] probes both dialects rather than
    /// assuming one.
    ///
    /// Naming an endpoint is still not the same as being able to reach it:
    /// this wallet has shipped an unreachable address once, with two
    /// independent faults (the wrong protocol, and a certificate that expired
    /// on 2026-08-11) each hiding the other.
    /// `crates/pecu-chain/tests/live_light.rs` connects to whatever is named
    /// here before it is believed.
    ///
    /// # What the server learns
    ///
    /// Which block ranges are asked for, and from where. Not which notes are
    /// yours — trial decryption happens on this machine and nothing about it
    /// leaves. Nor which transactions are yours: this wallet broadcasts through
    /// the **RPC node**, never through the light server, so the sharpest link —
    /// "the address that scanned is the address that then published a
    /// transaction" — is not available to it.
    ///
    /// It is still a real disclosure, and the interface says so where the
    /// address is shown rather than burying it here.
    pub fn light_server(&self) -> Option<&'static str> {
        match self {
            // Verus' own, reached over native gRPC. Operated by the project
            // rather than by anyone who worked on this wallet, which is the
            // property that matters for a default.
            Self::Testnet => Some("https://lightwalletd.verustest.net:8125"),
            // Nothing measured. A shielded balance read from a guessed server
            // is not a smaller version of a right one, it is a number with no
            // meaning.
            Self::Mainnet | Self::Other(_) => None,
        }
    }

    pub fn oracle(&self) -> Option<Oracle> {
        let oracle = |identity, content_key| {
            Some(Oracle {
                identity,
                content_key,
            })
        };
        match self {
            Self::Mainnet => oracle("VRSC@", "iSJ38vYX7qoCtotc9wBHb1vZdR3oTgoHCX"),
            Self::Testnet => oracle("VRSCTEST@", "iH51dFy7vF3LTRuVQvCTVu6QSbYfhTjek8"),
            // The PBaaS chains this build knows about. A chain that is not in
            // this list has no oracle **here** — which is a fact about this
            // wallet, not about the chain, and is why it is reported as its own
            // state rather than as silence.
            Self::Other(name) => match name.as_str() {
                "VARRR" | "vARRR" => oracle("vARRR@", "i8XmQTLRRvffV9XaNV1asxXduQMSypKksT"),
                "CHIPS" => oracle("CHIPS@", "iCgYC8eJm7raNJ2o6zYmfe8a2zeUF4e7tZ"),
                "VDEX" | "vDEX" => oracle("vDEX@", "iCNqwtiqG1ZgfrJcsPK92N8P9wVEnQVVus"),
                _ => None,
            },
        }
    }
}

impl core::fmt::Display for Network {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only testnet ships a lightwalletd, and mainnet must not pretend to.
    ///
    /// A guessed hostname here would produce a wallet that offers a shielded
    /// balance on VRSC and cannot deliver one — and a shielded balance read
    /// from the wrong server is not a smaller version of a right one, it is a
    /// number with no meaning.
    /// Testnet has one; nothing else does.
    ///
    /// An address shipped here is a promise that it can be reached, and this
    /// wallet has broken that promise once already — it named lightwalletd
    /// directly at a time when the transport could not speak its protocol, and
    /// behind a certificate that had expired. Both faults are gone (the second
    /// was fixed by its operator, the first by `crate::grpc`), and the address
    /// is back — but the rule that came out of it stands: the live test in
    /// `tests/live_light.rs` **connects** to whatever is named here, and
    /// nothing goes in this function until it has.
    #[test]
    fn only_testnet_names_a_light_server() {
        assert_eq!(
            Network::Testnet.light_server(),
            Some("https://lightwalletd.verustest.net:8125"),
        );
        assert_eq!(Network::Mainnet.light_server(), None);
        assert_eq!(Network::Other("VARRR".into()).light_server(), None);
    }

    /// Plaintext would leak which blocks are being fetched, and the SDK's
    /// transport refuses it for any non-loopback host. An `http://` server
    /// named here would fail at connect time instead of at review time.
    #[test]
    fn every_shipped_light_server_is_https() {
        for network in Network::shipped() {
            if let Some(url) = network.light_server() {
                assert!(
                    url.starts_with("https://"),
                    "{} ships a plaintext light server: {url}",
                    network.chain_name(),
                );
            }
        }
    }

    #[test]
    fn the_two_known_chains_are_recognised() {
        assert_eq!(Network::from_chain_name("VRSC"), Network::Mainnet);
        assert_eq!(Network::from_chain_name("VRSCTEST"), Network::Testnet);
        assert!(Network::from_chain_name("VRSC").is_mainnet());
        assert!(!Network::from_chain_name("VRSCTEST").is_mainnet());
    }

    /// Every shipped chain has an endpoint, an oracle and a name of its own.
    ///
    /// The three have to agree: a chain offered on a button with no node behind
    /// it is a dead choice, and one with no oracle cannot say whether it is
    /// taking conversions.
    #[test]
    fn every_shipped_chain_is_complete_and_distinct() {
        let mut urls = std::collections::BTreeSet::new();
        let mut titles = std::collections::BTreeSet::new();

        for chain in Network::shipped() {
            let nodes = chain.builtin_nodes();
            assert_eq!(nodes.len(), 1, "{chain} does not have exactly one endpoint");
            assert!(
                nodes[0].1.starts_with("https://"),
                "{chain} is offered over something other than https",
            );
            assert!(urls.insert(nodes[0].1), "{chain} reuses an endpoint");
            assert!(
                titles.insert(chain.title().to_string()),
                "{chain} reuses a title"
            );
            assert!(chain.oracle().is_some(), "{chain} has no oracle");
        }
    }

    /// Only Verus is mainnet. The PBaaS chains carry real coins and are not
    /// behind the mainnet spending guard, which is a decision worth seeing in a
    /// test rather than discovering.
    #[test]
    fn only_verus_itself_is_mainnet() {
        for chain in Network::shipped() {
            assert_eq!(
                chain.is_mainnet(),
                chain == Network::Mainnet,
                "{chain} disagrees about being mainnet",
            );
        }
    }

    /// A chain nobody ships has no endpoint, rather than somebody else's.
    #[test]
    fn an_unknown_chain_is_offered_nothing() {
        assert!(Network::Other("SOMEPBAAS".to_string())
            .builtin_nodes()
            .is_empty());
    }

    /// Every chain this build can ask about its own halts, and none of them
    /// share a key.
    ///
    /// Reusing one chain's content key on another reads whatever that chain
    /// happens to publish under it — which is either nothing, or somebody
    /// else's record parsed as an upgrade descriptor.
    #[test]
    fn no_two_chains_share_an_oracle_key() {
        let chains = [
            Network::Mainnet,
            Network::Testnet,
            Network::Other("VARRR".to_string()),
            Network::Other("CHIPS".to_string()),
            Network::Other("VDEX".to_string()),
        ];

        let mut keys = std::collections::BTreeSet::new();
        let mut identities = std::collections::BTreeSet::new();
        for chain in &chains {
            let oracle = chain
                .oracle()
                .unwrap_or_else(|| panic!("{chain} has no oracle"));
            assert!(
                keys.insert(oracle.content_key),
                "{chain} reuses a content key",
            );
            assert!(
                identities.insert(oracle.identity),
                "{chain} reuses an oracle identity",
            );
            assert!(oracle.content_key.starts_with('i'));
            assert!(oracle.identity.ends_with('@'));
        }
    }

    /// A chain nobody has configured is not a chain with nothing switched off.
    #[test]
    fn an_unknown_chain_has_nowhere_to_ask() {
        assert_eq!(Network::Other("SOMEPBAAS".to_string()).oracle(), None);
    }

    /// A chain this build does not know is carried, not rejected — and is not
    /// mainnet, so it cannot inherit mainnet's guard by accident.
    #[test]
    fn an_unknown_chain_is_carried_and_is_not_mainnet() {
        let other = Network::from_chain_name("SOMEPBAAS");
        assert_eq!(other, Network::Other("SOMEPBAAS".to_string()));
        assert!(!other.is_mainnet());
        assert_eq!(other.label(), "SOMEPBAAS");
    }

    #[test]
    fn only_the_chains_with_an_explorer_offer_one() {
        let txid = "abc123";
        assert_eq!(
            Network::Mainnet.explorer(txid).as_deref(),
            Some("https://explorer.verus.io/tx/abc123"),
        );
        assert_eq!(
            Network::Testnet.explorer(txid).as_deref(),
            Some("https://markets.chainvue.io/testnet/tx/abc123/"),
        );
        // A guessed hostname sends someone to a page that does not exist, or
        // to somebody else's chain.
        assert_eq!(Network::Other("SOMEPBAAS".to_string()).explorer(txid), None);
    }

    /// The two shipped directories are the ones that already exist on disk.
    ///
    /// If this ever starts deriving the path from the chain name, an existing
    /// wallet moves from `testnet/` to `vrsctest/` and its owner is shown an
    /// empty wallet with their money apparently gone.
    #[test]
    fn the_directory_names_are_the_ones_already_on_disk() {
        assert_eq!(Network::Mainnet.dir_name(), "mainnet");
        assert_eq!(Network::Testnet.dir_name(), "testnet");
    }

    /// A chain name is not guaranteed to be a usable path segment, and one that
    /// escaped would write outside the directory it was given.
    #[test]
    fn an_unknown_chain_name_cannot_escape_its_directory() {
        for (name, expected) in [
            ("SOMEPBAAS", "somepbaas"),
            ("../../etc", "------etc"),
            ("a/b", "a-b"),
            ("...", "chain"),
            ("", "chain"),
        ] {
            let dir = Network::Other(name.to_string()).dir_name();
            assert_eq!(dir, expected, "`{name}`");
            assert!(!dir.contains('/'), "`{name}` produced a path separator");
            assert!(!dir.contains('.'), "`{name}` kept a dot");
        }
    }

    /// The property that matters most: nothing about a hostname can make a
    /// chain look like testnet. Only the reported name decides.
    #[test]
    fn a_url_shaped_name_is_not_mistaken_for_a_chain() {
        for misleading in [
            "api.verustest.net",
            "testnet",
            "vrsctest",
            "VRSCTEST.example.com",
        ] {
            assert!(
                !matches!(Network::from_chain_name(misleading), Network::Testnet),
                "`{misleading}` must not be read as VRSCTEST — only an exact chain name counts",
            );
        }
    }
}

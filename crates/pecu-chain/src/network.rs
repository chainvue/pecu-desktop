//! Which chain we are on — as reported, never as assumed.

/// A Verus chain.
///
/// # This is derived from what the node says, never from the URL
///
/// A URL containing "test" proves nothing: an endpoint can be renamed,
/// misconfigured, or proxied. So the chain is read from what the daemon reports
/// about itself, through [`Network::from_chain_name`], and nothing in this
/// crate parses a hostname.
///
/// Getting this wrong is not cosmetic. A wallet that believes it is on testnet
/// while pointed at mainnet would sign a real transaction believing it was
/// worthless — the same key controls the same address on both chains, and
/// Verus has no branch-id-based network separation, so a signature made "for
/// testnet" is valid on mainnet as it stands.
///
/// # The name is cross-checked against the chain id, and what that buys
///
/// For the two chains this build pins an id for, the reported name alone is
/// not accepted: `Node::record_success` requires `ChainInfo::chain_id` to be
/// [`Network::chain_id`] as well, and refuses to believe a node whose two
/// statements about its own identity disagree.
///
/// Note what that pair is worth. It defeats a middlebox that relabels the name
/// on its way past and leaves the id alone, and a hand-rolled RPC shim that
/// answers with one chain's name beside another's id. It does **not** make a
/// hostile endpoint safe, and nothing written here could: both halves arrive in
/// the same `getinfo` reply, so a node willing to rewrite one is willing to
/// rewrite both. A wallet cannot establish which chain it is on from a single
/// untrusted source — a source cannot corroborate itself, and the node is the
/// only thing this crate has to ask. Raising that bar needs a second,
/// independently configured source, which is the same admission
/// [`crate::light`] makes about lightwalletd.
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

/// The three PBaaS chains this build ships, spelled the way their own daemons
/// spell them.
///
/// Read off live `getinfo` replies rather than chosen: `vapi.piratechain.com`
/// answers `"name":"vARRR"`, `api.vdex.to` answers `"name":"vDEX"`, and
/// `api.chips.cash` answers `"name":"CHIPS"`. One list, so that the spelling a
/// button offers and the spelling a node reports cannot drift apart —
/// [`Network::Other`] compares by exact string, and two spellings of one chain
/// are two chains.
const SHIPPED_PBAAS: [&str; 3] = ["vARRR", "CHIPS", "vDEX"];

impl Network {
    /// Read a network from `ChainInfo::name`.
    ///
    /// # The shipped PBaaS names are canonicalised here, and nowhere else
    ///
    /// [`Network::Other`] compares by exact string, so a wallet set to a chain
    /// spelled one way and a node reporting it spelled another are, to every
    /// comparison in this crate, on different chains: `Node::record_success`
    /// would mark such a node `WrongNetwork`, it would never reach `Online`,
    /// and the chain would read no balance and refuse every spend — while
    /// looking, on screen, like an endpoint that is simply serving the wrong
    /// thing. The three chains this build ships a button and an endpoint for
    /// are exactly the three where that would be a shipped fault rather than a
    /// user's typo, so their spelling is settled at the boundary and the rest
    /// of this file has one answer to match against.
    ///
    /// Case-insensitively, because a PBaaS chain's name comes from a currency
    /// definition rather than from a constant in the daemon, and Verus compares
    /// currency and identity names lowercased anyway — so a node that spells it
    /// differently from `getinfo` is still talking about the same chain.
    ///
    /// `VRSC` and `VRSCTEST` are matched exactly, and deliberately: those two
    /// are compile-time constants of the daemon, and anything else — `vrsc`
    /// from a hand-rolled shim, say — is a chain this build cannot vouch for
    /// and is carried as [`Network::Other`], which is the guarded answer.
    pub fn from_chain_name(name: &str) -> Self {
        match name {
            "VRSC" => Self::Mainnet,
            "VRSCTEST" => Self::Testnet,
            other => Self::Other(
                SHIPPED_PBAAS
                    .into_iter()
                    .find(|shipped| other.eq_ignore_ascii_case(shipped))
                    .unwrap_or(other)
                    .to_string(),
            ),
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

    /// The chain's own currency id, for the two chains this build pins one for.
    ///
    /// A root chain's currency id *is* the id of its name —
    /// `hash160(sha256d(lowercase(name)))`, the derivation
    /// [`verus_sdk::vdxf::root_namespace`] does offline and the one
    /// `verus_flows::balances::native_currency` already relies on. These are
    /// pinned as literals rather than derived at each call because a chain id
    /// is a constant a reviewer should be able to read straight off the page;
    /// `the_pinned_chain_ids_are_the_ones_the_derivation_produces` holds the
    /// literals to that derivation, so they are checkable facts rather than two
    /// magic strings nobody can audit.
    ///
    /// `None` for [`Network::Other`], and deriving one there would be wrong
    /// rather than merely missing: a PBaaS chain is not a root chain. `vARRR`
    /// is registered under VRSC, so its id is `identity_id("vARRR", VRSC)` and
    /// not `root_namespace("vARRR")` — deriving it here would reject every
    /// PBaaS node this build ships an endpoint for.
    ///
    /// # Two of the five shipped chains, and not the awkward three
    ///
    /// Say the coverage out loud, because it is the other half of what this
    /// check is worth. [`Network::shipped`] offers five chains and this pins
    /// ids for two of them. vARRR, CHIPS and vDEX carry real coins and are the
    /// three whose identity is never cross-checked at all, because the
    /// alternative was pinning ids nobody in this tree has. Pinning them is
    /// possible and is worth doing; `docs/LATER.md` §1b says why they cannot be
    /// derived from a name here and where the real values have to come from.
    ///
    /// What that gap is not is a gap in the spending guard.
    /// [`Network::may_be_real_money`] is true for all three, so the typed
    /// opt-in stands in front of a spend on each of them. What is missing here
    /// is corroboration of what a node says it is, not the confirmation the
    /// person gives.
    pub fn chain_id(&self) -> Option<&'static str> {
        match self {
            Self::Mainnet => Some("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV"),
            Self::Testnet => Some("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"),
            Self::Other(_) => None,
        }
    }

    /// Whether a spend here could cost somebody real money.
    ///
    /// True for everything except the one chain this build can positively say
    /// is worthless. That is the wrong way round from how it reads and it is
    /// deliberate: the question a spending guard has to answer is not "is this
    /// VRSC" but "could this signature move value", and only one of those two
    /// has a safe default. VRSCTEST coins come out of a faucet. Everything else
    /// is either known to carry real coins — vARRR, CHIPS and vDEX do, and this
    /// build ships an endpoint pointing at each of them, which is an invitation
    /// — or is a chain nobody here can vouch for, and those two deserve the
    /// same answer.
    ///
    /// # Why the rule is not "is this VRSC"
    ///
    /// "This chain is VRSC" is a true statement and the wrong question for a
    /// wallet: it reads "not VRSC" as "not real", which leaves the three PBaaS
    /// chains behind no confirmation at all while VRSC sits behind one. Nothing
    /// about vARRR makes a mistake there cheaper.
    ///
    /// The alternative rule considered was "gate every [`Network::Other`] chain
    /// this build ships an endpoint for, plus VRSC" — which keys a money guard
    /// on a packaging decision. Tidying an endpoint out of
    /// [`Network::builtin_nodes`] would then silently open the gate on that
    /// chain, and a hand-added endpoint for a chain this build has never heard
    /// of — the case where corroboration is weakest — would be the one case
    /// left ungated. This rule needs no re-audit when the shipped list changes.
    ///
    /// Matched arm by arm rather than written as `!= Testnet`, so that a second
    /// valueless chain — a private regtest, if one is ever shipped — is a line
    /// added here by somebody who thought about it, and a new variant is a
    /// compile error rather than a silent answer.
    ///
    /// The cost of being wrong in this direction is a developer on their own
    /// regtest chain typing one word, once per session, on a chain that turned
    /// out not to matter. The cost of being wrong in the other is somebody's
    /// money.
    pub fn may_be_real_money(&self) -> bool {
        match self {
            Self::Testnet => false,
            Self::Mainnet | Self::Other(_) => true,
        }
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
    /// this enum does not name. The spending guard keys on neither the variant
    /// nor on the chain being VRSC: it asks [`Network::may_be_real_money`],
    /// which is true for all three of these, because all three carry real coins
    /// and this build puts an endpoint for each of them on the node screen.
    ///
    /// Their names come from the private `SHIPPED_PBAAS` list rather than being
    /// written out again here, so a button can never offer a spelling
    /// [`Network::from_chain_name`] would not produce from the node's own
    /// answer — which would be a chain that is permanently `WrongNetwork`.
    pub fn shipped() -> Vec<Self> {
        let mut chains = vec![Self::Testnet, Self::Mainnet];
        chains.extend(SHIPPED_PBAAS.map(|name| Self::Other(name.to_string())));
        chains
    }

    /// What this chain is called on a button.
    ///
    /// [`Network::label`] answers with the chain's own name — `VRSCTEST`,
    /// `vARRR` — which is what a node reports and what has to be compared
    /// against. This is what a person recognises.
    pub fn title(&self) -> &str {
        match self {
            Self::Mainnet => "Verus",
            Self::Testnet => "Testnet",
            // One arm per chain, in the spelling `from_chain_name`
            // canonicalises to. A second spelling here would be a second answer
            // to a question that has one.
            Self::Other(name) => match name.as_str() {
                "vARRR" => "Pirate Chain",
                "CHIPS" => "CHIPS",
                "vDEX" => "vDEX",
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
                "vARRR" => &[("vARRR (public)", "https://vapi.piratechain.com/")],
                "CHIPS" => &[("CHIPS (public)", "https://api.chips.cash/")],
                "vDEX" => &[("vDEX (public)", "https://api.vdex.to/")],
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
                "vARRR" => oracle("vARRR@", "i8XmQTLRRvffV9XaNV1asxXduQMSypKksT"),
                "CHIPS" => oracle("CHIPS@", "iCgYC8eJm7raNJ2o6zYmfe8a2zeUF4e7tZ"),
                "vDEX" => oracle("vDEX@", "iCNqwtiqG1ZgfrJcsPK92N8P9wVEnQVVus"),
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
    use verus_sdk::verus_keys::{Address, AddressKind};

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
        assert_eq!(Network::Other("vARRR".into()).light_server(), None);
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

    /// The two pinned ids, held to the derivation they came from.
    ///
    /// A root chain's currency id *is* the id of its own name, so these are not
    /// arbitrary strings and nobody has to take them on trust: the same
    /// `root_namespace` the SDK derives a chain's native currency with produces
    /// them here, offline, at test time. Without this the pins would be two
    /// i-addresses copied from somewhere, and a single wrong character in one
    /// would quietly turn the cross-check for that chain into a check that
    /// rejects every honest node — a failure that looks like an outage.
    ///
    /// The comparison is between whole strings, not between the 20 bytes
    /// underneath them, because the string is what production compares: the pin
    /// is held against `ChainInfo::chain_id` verbatim. Parsing the pin and
    /// checking only its hash would accept a pin written with the wrong version
    /// byte — the same 20 bytes spelled `R…` instead of `i…` is a valid address
    /// and a chain id no daemon ever sends — which is the outage this test is
    /// here to make impossible.
    ///
    /// [`Network::Other`] is left out because it pins nothing, which is its own
    /// deliberate decision and is tested by
    /// `a_pbaas_node_is_still_believed_because_no_id_is_pinned_for_it`.
    #[test]
    fn the_pinned_chain_ids_are_the_ones_the_derivation_produces() {
        for chain in [Network::Mainnet, Network::Testnet] {
            let derived = verus_sdk::vdxf::root_namespace(chain.chain_name())
                .expect("a chain name is a root name");

            assert_eq!(
                Address::new(AddressKind::Identity, derived.to_bytes()).to_string(),
                chain.chain_id().expect("both named chains pin an id"),
                "{chain}",
            );
        }
    }

    /// The two named chains map to variants of their own, and so can never
    /// arrive as an unknown one.
    ///
    /// Worth stating as well as the equality, because the spending rule turns
    /// on the variant and `Other` is reachable from any string: a node calling
    /// itself `VRSCTEST` is judged as testnet, which is the whole reason the
    /// name/chain-id cross-check stands in front of this.
    #[test]
    fn the_two_known_chains_are_recognised() {
        assert_eq!(Network::from_chain_name("VRSC"), Network::Mainnet);
        assert_eq!(Network::from_chain_name("VRSCTEST"), Network::Testnet);
        assert!(!matches!(
            Network::from_chain_name("VRSC"),
            Network::Other(_)
        ));
        assert!(!matches!(
            Network::from_chain_name("VRSCTEST"),
            Network::Other(_)
        ));
    }

    /// Every name a button can send survives the round trip through the
    /// function that reads a node's answer.
    ///
    /// The invariant, rather than the specific spelling: the two lists are one
    /// list today, and this is what fails if they are ever separated again.
    /// `Other` compares by exact string, so a button offering one spelling
    /// while the parser produces another is a chain that is `WrongNetwork`
    /// forever — never `Online`, so no balance and no spend, on a chain this
    /// build ships an endpoint for. The spelling itself is pinned by the
    /// sibling below, against what the daemons actually answer.
    #[test]
    fn every_shipped_chain_survives_the_round_trip_through_its_own_name() {
        for chain in Network::shipped() {
            assert_eq!(
                Network::from_chain_name(chain.chain_name()),
                chain,
                "{chain} is offered under a name that parses back as something else",
            );
        }
    }

    /// The daemons spell the PBaaS chains with a lowercase leading `v`, and
    /// this build has to accept whatever case one of them uses.
    ///
    /// The three spellings on the left are live `getinfo` answers, captured
    /// 2026-08-22. The rest are the same names shouted and muttered: the name
    /// is a currency definition rather than a constant in the daemon, and Verus
    /// compares such names lowercased, so a node spelling it differently is
    /// still talking about the same chain and must not be refused as another
    /// one.
    #[test]
    fn a_shipped_pbaas_chain_is_recognised_however_it_spells_itself() {
        for (reported, canonical) in [
            ("vARRR", "vARRR"),
            ("VARRR", "vARRR"),
            ("varrr", "vARRR"),
            ("CHIPS", "CHIPS"),
            ("chips", "CHIPS"),
            ("vDEX", "vDEX"),
            ("VDEX", "vDEX"),
        ] {
            assert_eq!(
                Network::from_chain_name(reported),
                Network::Other(canonical.to_string()),
                "a node reporting `{reported}` was read as a different chain",
            );
        }

        // Only the shipped three. A chain nobody here can vouch for is carried
        // exactly as it spelled itself — there is no canonical form to know.
        assert_eq!(
            Network::from_chain_name("SomePbaas"),
            Network::Other("SomePbaas".to_string()),
        );
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

    /// The rule the spending guard applies, in one place a reader can check it.
    ///
    /// Testnet is the only chain this build can positively say is worthless, so
    /// it is the only one a spend passes through untouched. vARRR, CHIPS and
    /// vDEX carry real coins and this build ships an endpoint for each; a chain
    /// nobody here has heard of gets the same answer, because "we cannot vouch
    /// for it" is not the same as "it is worthless". The argument for the rule
    /// lives on [`Network::may_be_real_money`], where the decision is made.
    #[test]
    fn only_testnet_is_free_of_the_spending_guard() {
        let mut chains = Network::shipped();
        chains.push(Network::Other("SOMEPBAAS".to_string()));

        for chain in chains {
            assert_eq!(
                chain.may_be_real_money(),
                chain != Network::Testnet,
                "{chain} disagrees about whether a spend on it could cost money",
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
        let mut keys = std::collections::BTreeSet::new();
        let mut identities = std::collections::BTreeSet::new();
        for chain in &Network::shipped() {
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

    /// A chain this build does not know is carried, not rejected — and it
    /// inherits the spending guard on purpose. Being unable to vouch for a
    /// chain is the strongest reason to ask for the confirmation, not a reason
    /// to skip it.
    #[test]
    fn an_unknown_chain_is_carried_and_is_still_guarded() {
        let other = Network::from_chain_name("SOMEPBAAS");
        assert_eq!(other, Network::Other("SOMEPBAAS".to_string()));
        assert!(other.may_be_real_money());
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

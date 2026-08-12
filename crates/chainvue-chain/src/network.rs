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

impl core::fmt::Display for Network {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_known_chains_are_recognised() {
        assert_eq!(Network::from_chain_name("VRSC"), Network::Mainnet);
        assert_eq!(Network::from_chain_name("VRSCTEST"), Network::Testnet);
        assert!(Network::from_chain_name("VRSC").is_mainnet());
        assert!(!Network::from_chain_name("VRSCTEST").is_mainnet());
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

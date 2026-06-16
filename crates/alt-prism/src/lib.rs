//! The prism layer's Tier 1 framework: encoding-aware *reversible*
//! decomposition of a file into parts that the chunk store can dedup at a
//! finer grain than the whole file (a container's members, a stream's
//! inflated bytes, an image's layers).
//!
//! **Tier 1 iron law** (design/prisms.md §1): decomposition is a pure
//! storage gain and fidelity *never* depends on a prism being correct. The
//! pipeline here decomposes, immediately recomposes, and compares against
//! the original bytes — only a byte-exact round trip is accepted; anything
//! else falls back to Tier 0 (store the original verbatim). A buggy or
//! adversarial prism can at worst waste the attempt, never corrupt data.
//!
//! This crate is the registry + pipeline (the steel). Individual prisms
//! (deflate strip, zip, png, …) are stones that implement [`Prism`].

/// Identifies which prism produced a decomposition, so recomposition uses
/// the matching one. Stable across versions — never reuse a retired id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrismId(pub u16);

/// A reversible decomposition of one file: the parts (each stored via the
/// chunk store, deduplicated) plus a prism-private recipe describing how to
/// reassemble the exact original bytes from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decomposition {
    pub recipe: Vec<u8>,
    pub parts: Vec<Vec<u8>>,
}

/// One encoding-aware decomposer. A prism is a business-agnostic stone:
/// it must be independently fuzzable and bound its own resource use
/// (decompression bombs); it must never panic on adversarial input.
///
/// `Send + Sync` is required so a [`Registry`] (and any store embedding
/// one) can sit inside an `Arc<Mutex<…>>` — the daemon holds the odb that
/// way. Stateless prisms satisfy this trivially; any prism holding interior
/// state must keep it thread-safe.
pub trait Prism: Send + Sync {
    /// This prism's stable identity.
    fn id(&self) -> PrismId;

    /// Attempts to decompose `input`. `None` means "not my format" (a fast,
    /// cheap reject) — it does not mean failure. The returned parts and
    /// recipe must let [`recompose`](Prism::recompose) rebuild `input`
    /// byte-for-byte; the pipeline verifies this, so a prism may return a
    /// decomposition it is unsure about and let the round trip judge it.
    fn decompose(&self, input: &[u8]) -> Option<Decomposition>;

    /// Rebuilds the original bytes from a recipe and the part bytes (in the
    /// order [`decompose`](Prism::decompose) produced them). `None` on any
    /// malformed input — never a panic.
    fn recompose(&self, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>>;
}

/// The set of prisms tried at ingest, in priority order (hot formats first,
/// per design/prisms.md §-1.5).
#[derive(Default)]
pub struct Registry {
    prisms: Vec<Box<dyn Prism + Send + Sync>>,
}

/// A Tier 1 acceptance: the prism that produced it and its verified
/// decomposition. Holding one means the round trip already succeeded.
#[derive(Debug)]
pub struct Tier1 {
    pub prism: PrismId,
    pub decomposition: Decomposition,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a prism. Order is priority: earlier prisms are tried first.
    pub fn register(&mut self, prism: Box<dyn Prism + Send + Sync>) {
        self.prisms.push(prism);
    }

    /// Tier 1 ingest decision. Tries each prism; the first whose
    /// decomposition recomposes **byte-exactly** to `input` wins. Returns
    /// `None` when no prism applies or none round-trips — the caller then
    /// stores `input` as Tier 0. The fidelity check happens here, before
    /// anything is stored: that is the iron law.
    pub fn decompose_verified(&self, input: &[u8]) -> Option<Tier1> {
        for prism in &self.prisms {
            let Some(decomposition) = prism.decompose(input) else {
                continue;
            };
            let part_refs: Vec<&[u8]> = decomposition.parts.iter().map(Vec::as_slice).collect();
            match prism.recompose(&decomposition.recipe, &part_refs) {
                Some(rebuilt) if rebuilt == input => {
                    return Some(Tier1 {
                        prism: prism.id(),
                        decomposition,
                    });
                }
                // decomposed but did not round-trip: reject this prism (a
                // buggy decompose or a foreign encoder) and try the next
                _ => continue,
            }
        }
        None
    }

    /// Recomposes a Tier 1 file: looks up the prism by id and rebuilds the
    /// original bytes from the recipe and parts.
    pub fn recompose(&self, prism: PrismId, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
        self.prisms
            .iter()
            .find(|p| p.id() == prism)?
            .recompose(recipe, parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A faithful demo prism: splits a file in two at a recorded offset.
    /// Stands in for a real container prism to exercise the pipeline.
    struct SplitPrism;
    impl Prism for SplitPrism {
        fn id(&self) -> PrismId {
            PrismId(1)
        }
        fn decompose(&self, input: &[u8]) -> Option<Decomposition> {
            if input.len() < 2 {
                return None; // nothing to split
            }
            let mid = input.len() / 2;
            Some(Decomposition {
                recipe: (mid as u32).to_le_bytes().to_vec(),
                parts: vec![input[..mid].to_vec(), input[mid..].to_vec()],
            })
        }
        fn recompose(&self, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
            let mid = u32::from_le_bytes(recipe.try_into().ok()?) as usize;
            let [head, tail] = parts else { return None };
            if head.len() != mid {
                return None;
            }
            Some([*head, *tail].concat())
        }
    }

    /// A broken prism whose recompose drops a byte — the round trip must
    /// reject it so a bug can never reach storage.
    struct LyingPrism;
    impl Prism for LyingPrism {
        fn id(&self) -> PrismId {
            PrismId(2)
        }
        fn decompose(&self, input: &[u8]) -> Option<Decomposition> {
            Some(Decomposition {
                recipe: Vec::new(),
                parts: vec![input.to_vec()],
            })
        }
        fn recompose(&self, _recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
            let mut out = parts[0].to_vec();
            out.pop(); // corrupts the round trip
            Some(out)
        }
    }

    #[test]
    fn faithful_prism_is_accepted_and_recomposes() {
        let mut reg = Registry::new();
        reg.register(Box::new(SplitPrism));
        let input = b"hello, prism world";

        let t1 = reg
            .decompose_verified(input)
            .expect("split must round-trip");
        assert_eq!(t1.prism, PrismId(1));
        assert_eq!(t1.decomposition.parts.len(), 2);

        let parts: Vec<&[u8]> = t1.decomposition.parts.iter().map(Vec::as_slice).collect();
        let back = reg
            .recompose(t1.prism, &t1.decomposition.recipe, &parts)
            .unwrap();
        assert_eq!(back, input, "recompose must reproduce the original");
    }

    #[test]
    fn a_prism_that_does_not_round_trip_is_rejected() {
        let mut reg = Registry::new();
        reg.register(Box::new(LyingPrism)); // would corrupt if trusted
        // the iron law: no byte-exact round trip => Tier 0 (None), never
        // a silently wrong acceptance
        assert!(reg.decompose_verified(b"important data").is_none());
    }

    #[test]
    fn falls_through_to_a_later_prism_when_the_first_declines_or_lies() {
        let mut reg = Registry::new();
        reg.register(Box::new(LyingPrism)); // decomposes but never round-trips
        reg.register(Box::new(SplitPrism)); // faithful
        let input = b"second prism saves the day";
        let t1 = reg
            .decompose_verified(input)
            .expect("split should still win");
        assert_eq!(t1.prism, PrismId(1));
    }

    #[test]
    fn no_prism_applies_is_tier_0() {
        let reg = Registry::new(); // empty registry
        assert!(reg.decompose_verified(b"anything").is_none());
    }
}

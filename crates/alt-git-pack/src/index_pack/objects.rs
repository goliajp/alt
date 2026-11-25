//! What a walk is given of the pack it goes through.

use crate::PackError;

/// The pack a walk goes through, and what is done with what it makes.
pub trait Objects {
    type Id: Ord + Copy;
    type Error: From<PackError>;

    /// What the entry inflates to: the object of a base that is no delta,
    /// the instructions of a delta. `scratch` is there to inflate into.
    fn payload<'a>(
        &'a mut self,
        entry: u32,
        scratch: &'a mut Vec<u8>,
    ) -> Result<&'a [u8], Self::Error>;

    /// Whether the delta was resolved before, through another entry of
    /// the object it is made of, or is another walk's to resolve. A delta
    /// this says no of is resolved by the walk that asked: where several
    /// walks go through one pack, it is said of each delta to one of them.
    fn reached(&self, delta: u32) -> bool;

    /// Takes the object a delta made, of the base `of` and `depth` deltas
    /// below a plain entry, and gives its id.
    fn made(
        &mut self,
        delta: u32,
        of: u32,
        depth: u32,
        object: &[u8],
    ) -> Result<Self::Id, Self::Error>;

    /// Says of the delta `delta`, made of the entry `of`, that it lies
    /// deeper than the walk goes. An error ends the walk. Otherwise the
    /// delta is left unresolved and the walk goes on beside it: whoever
    /// says so resolves it another way, and has to take back that it
    /// was reached.
    fn beyond(&mut self, _delta: u32, _of: u32) -> Result<(), Self::Error> {
        Err(PackError::Limit("delta depth").into())
    }
}

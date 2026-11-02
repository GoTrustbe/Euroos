//! Interrupt vectors for userspace drivers: a small allocator.
//!
//! The kernel reserves a contiguous range of IDT vectors for devices driven
//! from ring 3 and hands them out through a [`VectorPool`], never through a
//! per-driver constant. The rules:
//!
//! - a vector has at most one owner: a claim on a vector that another owner
//!   holds is refused ([`VecError::Taken`]);
//! - an owner holds at most one vector: asking again returns the one it has
//!   (a restarted driver keeps its vector, the device's MSI-X entry stays valid);
//! - only the owner can give a vector back.
//!
//! An owner is a small number chosen by the kernel (its driver index). When
//! the pool is exhausted the caller gets [`VecError::Exhausted`] and runs its
//! device without interrupts (polled) instead of sharing a vector.

/// Largest pool this type can hold.
pub const MAX_VECTORS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VecError {
    /// Every vector of the pool has an owner.
    Exhausted,
    /// The vector is outside the pool.
    NotInPool,
    /// The vector belongs to another owner.
    Taken { owner: u8 },
    /// The owner already holds another vector.
    AlreadyHolds { vector: u8 },
    /// The caller does not hold that vector.
    NotOwner,
}

/// The vectors `base .. base + len` and who owns each of them.
#[derive(Clone, Copy, Debug)]
pub struct VectorPool {
    base: u8,
    len: u8,
    owner: [Option<u8>; MAX_VECTORS],
}

impl VectorPool {
    /// A pool of `len` vectors from `base` on (`len` is capped at
    /// [`MAX_VECTORS`] and at the end of the IDT).
    pub const fn new(base: u8, len: u8) -> VectorPool {
        let mut n = if (len as usize) > MAX_VECTORS { MAX_VECTORS as u8 } else { len };
        if base as u16 + n as u16 > 256 {
            n = (256 - base as u16) as u8;
        }
        VectorPool { base, len: n, owner: [None; MAX_VECTORS] }
    }

    pub fn base(&self) -> u8 {
        self.base
    }

    pub fn len(&self) -> u8 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Is `vector` one of the pool's?
    pub fn contains(&self, vector: u8) -> bool {
        vector >= self.base && ((vector - self.base) as usize) < self.len as usize
    }

    /// Index of `vector` inside the pool.
    pub fn index(&self, vector: u8) -> Option<usize> {
        self.contains(vector).then(|| (vector - self.base) as usize)
    }

    pub fn owner_of(&self, vector: u8) -> Option<u8> {
        self.index(vector).and_then(|i| self.owner[i])
    }

    pub fn vector_of(&self, owner: u8) -> Option<u8> {
        (0..self.len as usize).find(|&i| self.owner[i] == Some(owner)).map(|i| self.base + i as u8)
    }

    /// The first free vector for `owner`, or the one it already holds.
    pub fn alloc(&mut self, owner: u8) -> Result<u8, VecError> {
        if let Some(v) = self.vector_of(owner) {
            return Ok(v);
        }
        let i = (0..self.len as usize).find(|&i| self.owner[i].is_none()).ok_or(VecError::Exhausted)?;
        self.owner[i] = Some(owner);
        Ok(self.base + i as u8)
    }

    /// A fixed vector for `owner`: refused when another owner has it, or when
    /// `owner` already holds a different one.
    pub fn claim(&mut self, vector: u8, owner: u8) -> Result<u8, VecError> {
        let i = self.index(vector).ok_or(VecError::NotInPool)?;
        match self.owner[i] {
            Some(o) if o == owner => return Ok(vector),
            Some(o) => return Err(VecError::Taken { owner: o }),
            None => {}
        }
        if let Some(v) = self.vector_of(owner) {
            return Err(VecError::AlreadyHolds { vector: v });
        }
        self.owner[i] = Some(owner);
        Ok(vector)
    }

    /// Give `vector` back. Only its owner can.
    pub fn release(&mut self, vector: u8, owner: u8) -> Result<(), VecError> {
        let i = self.index(vector).ok_or(VecError::NotInPool)?;
        if self.owner[i] != Some(owner) {
            return Err(VecError::NotOwner);
        }
        self.owner[i] = None;
        Ok(())
    }

    /// Number of vectors with an owner.
    pub fn in_use(&self) -> usize {
        self.owner[..self.len as usize].iter().filter(|o| o.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_hands_out_distinct_vectors_then_runs_out() {
        let mut p = VectorPool::new(0x4C, 4);
        let v: Vec<u8> = (0..4).map(|o| p.alloc(o).unwrap()).collect();
        assert_eq!(v, [0x4C, 0x4D, 0x4E, 0x4F]);
        assert_eq!(p.alloc(9), Err(VecError::Exhausted));
        assert_eq!(p.in_use(), 4);
    }

    #[test]
    fn an_owner_keeps_its_vector() {
        let mut p = VectorPool::new(0x4C, 4);
        assert_eq!(p.alloc(1), Ok(0x4C));
        assert_eq!(p.alloc(2), Ok(0x4D));
        // A restart asks again: the same vector, nothing new taken.
        assert_eq!(p.alloc(1), Ok(0x4C));
        assert_eq!(p.in_use(), 2);
    }

    #[test]
    fn a_taken_vector_is_refused_to_another_owner() {
        let mut p = VectorPool::new(0x4C, 4);
        assert_eq!(p.alloc(0), Ok(0x4C));
        assert_eq!(p.claim(0x4C, 3), Err(VecError::Taken { owner: 0 }));
        assert_eq!(p.owner_of(0x4C), Some(0));
        assert_eq!(p.claim(0x4D, 3), Ok(0x4D));
        assert_eq!(p.claim(0x4D, 3), Ok(0x4D));
        // One vector per owner.
        assert_eq!(p.claim(0x4E, 3), Err(VecError::AlreadyHolds { vector: 0x4D }));
        assert_eq!(p.claim(0x50, 4), Err(VecError::NotInPool));
        assert_eq!(p.claim(0x4B, 4), Err(VecError::NotInPool));
    }

    #[test]
    fn only_the_owner_releases() {
        let mut p = VectorPool::new(0x4C, 2);
        let v = p.alloc(5).unwrap();
        assert_eq!(p.release(v, 6), Err(VecError::NotOwner));
        assert_eq!(p.release(0x60, 5), Err(VecError::NotInPool));
        assert_eq!(p.release(v, 5), Ok(()));
        assert_eq!(p.owner_of(v), None);
        assert_eq!(p.release(v, 5), Err(VecError::NotOwner));
        // Freed: the next owner may have it.
        assert_eq!(p.alloc(6), Ok(v));
    }

    #[test]
    fn pool_bounds() {
        let p = VectorPool::new(0xFE, 8);
        assert_eq!(p.len(), 2);
        assert!(p.contains(0xFF) && !p.contains(0xFD));
        let p = VectorPool::new(0x20, 200);
        assert_eq!(p.len() as usize, MAX_VECTORS);
        let mut p = VectorPool::new(0x4C, 0);
        assert!(p.is_empty());
        assert_eq!(p.alloc(0), Err(VecError::Exhausted));
    }
}

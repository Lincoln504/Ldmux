//! The one source of randomness the simulator has, so a run is a function of its seed.
//!
//! A replayed failure is worth nothing if the replay draws different numbers, and every interesting
//! property of this harness — a collision that only happens at one forward delay, a loss pattern
//! that empties a window — depends on draws. So the draws are a PCG-XSH-RR generator, seeded once
//! per mesh, and the whole state is one `u64`.
//!
//! It is deliberately **not** `rand`: this crate is linked into the Android library and carries no
//! dependencies beyond the ones the wire needs. `getrandom` would also make a run irreproducible
//! unless seeded anyway, so the dependency would buy nothing.

/// A PCG-XSH-RR generator (O'Neill, 2014), 64-bit state, 32-bit output.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
    /// The stream selector. Odd, so consecutive states never fall into a short cycle.
    inc: u64,
}

impl Rng {
    /// A generator whose whole future is decided by `seed`.
    ///
    /// The two constants are the PCG reference ones: `6364136223846793005` is the LCG multiplier,
    /// and the increment is forced odd by `| 1`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut rng = Self {
            state: 0,
            inc: (seed ^ 0x9E37_79B9_7F4A_7C15) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    /// The next 32 bits.
    pub fn next_u32(&mut self) -> u32 {
        let previous = self.state;
        self.state = previous
            .wrapping_mul(6364136223846793005)
            .wrapping_add(self.inc);
        // PCG-XSH-RR: a 27-bit xorshift of the old state, truncated to 32 bits, rotated by its own
        // top five bits. The truncation is part of the definition, not a loss — taking the whole 37
        // remaining bits instead would make every draw exceed `u32` and collapse the stream.
        let shifted = ((previous >> 18) ^ previous) >> 27;
        let xorshifted = (shifted & 0xFFFF_FFFF) as u32;
        let rot = u32::try_from(previous >> 59).unwrap_or(0);
        xorshifted.rotate_right(rot & 31)
    }

    /// A value in `0..n`, the shape the firmware's `getRNG()->nextInt(0, n)` has.
    ///
    /// The modulo bias of a 32-bit draw over a small `n` is 1 in 2^32 per unit and is stated rather
    /// than corrected: correcting it would change the value a given seed produces, and reproducing
    /// a run matters more than a bias no test can resolve. `n == 0` answers `0`, which is what the
    /// firmware's `nextInt(0, 3*t + 1)` cannot ask but a caller might.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.next_u32() % n }
    }

    /// True with probability `p`, clamped into `0.0..=1.0`.
    pub fn chance(&mut self, p: f64) -> bool {
        if p <= 0.0 {
            return false;
        }
        if p >= 1.0 {
            return true;
        }
        // 24 bits of resolution is finer than any loss figure this harness is asked to model and
        // keeps the comparison free of floating-point edge cases at 1.0.
        f64::from(self.next_u32() >> 8) / f64::from(1u32 << 24) < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole harness rests on: the same seed produces the same stream. A test that
    /// pins a collision would be worthless without it.
    #[test]
    fn the_same_seed_produces_the_same_stream() {
        let mut one = Rng::new(11);
        let first: Vec<u32> = (0..32).map(|_| one.next_u32()).collect();
        let mut other = Rng::new(11);
        let second: Vec<u32> = (0..32).map(|_| other.next_u32()).collect();
        assert_eq!(first, second, "two generators from one seed diverged");
    }

    /// Two seeds must not produce the same stream, or a "different seed, same result" comparison a
    /// test might rely on would be meaningless.
    #[test]
    fn different_seeds_produce_different_streams() {
        let mut one = Rng::new(1);
        let mut other = Rng::new(2);
        let a: Vec<u32> = (0..16).map(|_| one.next_u32()).collect();
        let b: Vec<u32> = (0..16).map(|_| other.next_u32()).collect();
        assert_ne!(a, b);
    }

    /// `below` stays inside its bound, and `chance(0.0)`/`chance(1.0)` are the two answers a caller
    /// depends on being exact — a loss figure of 0 must never drop a packet.
    #[test]
    fn a_bound_is_respected_and_the_extremes_are_exact() {
        let mut rng = Rng::new(7);
        for _ in 0..1000 {
            assert!(rng.below(5) < 5);
        }
        for _ in 0..1000 {
            assert!(!rng.chance(0.0));
            assert!(rng.chance(1.0));
        }
    }

    /// The draws are spread across the whole unit interval, which is the property a loss figure
    /// depends on. A generator that only ever produced small values would drop every packet at a
    /// quarter loss, and the failure would look like the mesh rather than like the generator.
    #[test]
    fn the_draws_are_spread_over_the_unit_interval() {
        let mut rng = Rng::new(21);
        let mut low = 0u32;
        let mut high = 0u32;
        for _ in 0..10_000 {
            if rng.chance(0.5) {
                low += 1;
            } else {
                high += 1;
            }
        }
        assert!(
            low > 4_000 && high > 4_000,
            "a half-chance gave {low} true and {high} false out of 10000"
        );
    }
}

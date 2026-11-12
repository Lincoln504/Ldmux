//! How long one packet occupies the channel, for the tuple a radio is tuned to.
//!
//! This is Semtech's time-on-air formula for the SX126x, which is the arithmetic the whole radio
//! route budgets against: the hourly allowance (`docs/AIRTIME.md`), the repeater's own forward
//! delay (`getRetransmitDelay`, `examples/simple_repeater/MyMesh.cpp:597-605`), and every
//! retransmission deadline in the session. It lives here, at the root of the shared vocabulary,
//! because both the host's companion driver and the phone's engine need the same number and
//! neither may own it.
//!
//! **`crates/companion/src/codec.rs::time_on_air_ms` is the reference implementation**, and it is
//! the one that ships today. This function is its twin, deliberately so: the companion crate's
//! framing and this crate's endpoint are being separated by stage `S0` of `docs/RADIO-PLAN.md`, and
//! until the companion's codec moves here there are two copies. They cannot drift silently —
//! `crates/companion/tests/airtime_agreement.rs` compares them across a range of payload sizes at
//! the public preset and fails on a tenth of a millisecond of disagreement. When the codec does
//! move, that test is what says this copy is now the only one.

use core::fmt;

/// The four numbers that define a MeshCore channel, less the frequency.
///
/// Frequency is not here because it does not enter the airtime arithmetic: two tuples that differ
/// only in `freq_khz` occupy the channel for exactly as long. Keeping it out is what stops a
/// caller from believing a frequency change bought or cost time.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Modulation {
    /// Bandwidth in kHz. 62.5 is the public USA/Canada preset, 500 the home bench.
    pub bw_khz: f64,
    /// Spreading factor, 7..=12.
    pub sf: u8,
    /// Coding rate as the firmware's denominator index: 5 is 4/5, 8 is 4/8.
    pub cr: u8,
}

impl Modulation {
    /// The public USA/Canada preset: 62.5 kHz, SF7, CR5.
    ///
    /// This is the tuple every figure in `docs/AIRTIME.md` is quoted at, and the one the field
    /// sessions of 2026-11-10 ran on.
    pub const PUBLIC_PRESET: Self = Self {
        bw_khz: 62.5,
        sf: 7,
        cr: 5,
    };
}

impl fmt::Display for Modulation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} kHz SF{} CR{}", self.bw_khz, self.sf, self.cr)
    }
}

/// Time on air for one packet of `payload_len` bytes at `m`, in milliseconds.
///
/// `payload_len` is the **whole packet**: header, path-length byte, path and payload. That is what
/// the radio transmits, and it is what `getEstAirtimeFor` is handed in the firmware
/// (`packet->getPathByteLen() + packet->payload_len + 2`).
///
/// The shape, term by term, is the datasheet's:
///
/// - `symbol_ms` is one LoRa symbol, `2^SF / BW`.
/// - The preamble is **32 symbols at SF8 and below, 16 above**
///   (`preambleLengthForSF`, `src/helpers/radiolib/RadioLibWrappers.h:56`). Eight was used once and
///   understated a record by about 50 ms at 62.5 kHz, an 8 % error in the hour's allowance.
/// - Low-data-rate optimisation is automatic in RadioLib and is on once a symbol reaches 16 ms
///   (`SX126x::setModulationParams`), which halves the denominator in the payload-symbol count.
/// - An explicit header and a CRC add 20 header symbols and 16 CRC bits, folded into the `+ 28`
///   and `+ 16` (`SX126x.cpp:755-759`).
///
/// A tuple this cannot price (`bw_khz` zero, `sf` zero) returns `0.0` rather than a guess: no radio
/// is tuned to it, so the honest answer is that the figure is unknown.
#[must_use]
pub fn airtime_ms(m: Modulation, payload_len: usize) -> f64 {
    let bw_hz = m.bw_khz * 1000.0;
    if bw_hz <= 0.0 || m.sf == 0 {
        return 0.0;
    }
    let sf_f = f64::from(m.sf);
    // The firmware's coding rate is the denominator RadioLib reads it as: 5 is 4/5, and the symbol
    // multiplier is that number, which is the datasheet's `CR + 4` with CR counted from 0.
    let cr_f = f64::from(m.cr).clamp(5.0, 8.0);
    let symbol_ms = (2f64.powf(sf_f)) / bw_hz * 1000.0;
    let preamble = if m.sf <= 8 { 32.0 } else { 16.0 };
    let de = if symbol_ms >= 16.0 { 1.0 } else { 0.0 };
    let numerator = 8.0 * payload_len as f64 - 4.0 * sf_f + 28.0 + 16.0;
    let denominator = 4.0 * (sf_f - 2.0 * de);
    let payload_symbols = 8.0 + (numerator / denominator).ceil().max(0.0) * cr_f;
    (preamble + 4.25 + payload_symbols) * symbol_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two figures `docs/AIRTIME.md` and `docs/RADIO-AUDIT-REVIEW.md` §6.1 quote as measured,
    /// at the public preset. A change to the preamble rule or the coding-rate sense moves them,
    /// which is the point of pinning them here.
    #[test]
    fn the_recorded_public_preset_figures_are_reproduced() {
        let m = Modulation::PUBLIC_PRESET;
        assert!(
            (airtime_ms(m, 42) - 223.7).abs() < 0.2,
            "42 B was measured at 223.7 ms, computed {}",
            airtime_ms(m, 42)
        );
        assert!(
            (airtime_ms(m, 172) - 602.6).abs() < 0.2,
            "172 B was measured at 602.6 ms, computed {}",
            airtime_ms(m, 172)
        );
    }

    /// A bigger spreading factor is slower at the same bandwidth, and this is the only ordering
    /// claim the budget depends on: a tuple that got faster when the factor rose would silently
    /// overrun every deadline derived from it.
    #[test]
    fn a_higher_spreading_factor_costs_more_air() {
        let mut previous = 0.0;
        for sf in 7..=12 {
            let m = Modulation {
                bw_khz: 125.0,
                sf,
                cr: 5,
            };
            let now = airtime_ms(m, 100);
            assert!(
                now > previous,
                "SF{sf} cost {now} ms, which is not more than the {previous} ms before it"
            );
            previous = now;
        }
    }

    /// No radio is tuned to a zero bandwidth or a zero spreading factor, so the price is unknown
    /// rather than free. Returning a number here would let a budget divide by it.
    #[test]
    fn an_unpriceable_tuple_is_zero_and_not_a_guess() {
        let no_bandwidth = Modulation {
            bw_khz: 0.0,
            sf: 7,
            cr: 5,
        };
        let no_factor = Modulation {
            bw_khz: 62.5,
            sf: 0,
            cr: 5,
        };
        assert_eq!(airtime_ms(no_bandwidth, 100), 0.0);
        assert_eq!(airtime_ms(no_factor, 100), 0.0);
    }
}

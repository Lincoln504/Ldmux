//! The airtime arithmetic must be one number, in two crates, for as long as there are two copies.
//!
//! `crates/companion/src/codec.rs::time_on_air_ms` is what ships today: it prices a record for the
//! hourly allowance, for the retransmission ladders and for the repeater's own forward delay.
//! `ldmux_mesh::airtime::airtime_ms` is the copy that is moving to the shared crate as stage `S0` of
//! `docs/RADIO-PLAN.md` separates the companion's framing from the endpoint's.
//!
//! Two copies of a formula is a defect waiting to happen, and it already happened once: the
//! companion used eight preamble symbols where the firmware uses thirty-two, which understated a
//! record by about 50 ms at 62.5 kHz — an 8 % error in the hour's allowance. This test is the thing
//! that makes the duplication survivable: every size, at every tuple a deployment uses, to a tenth
//! of a millisecond.

use ldmux_companion::codec::time_on_air_ms;
use ldmux_mesh::airtime::{airtime_ms, Modulation};

/// The tuples a real deployment runs on, from `docs/AIRTIME.md`: the public USA/Canada preset, the
/// home bench, and the two European sub-bands whose allowances differ.
const TUPLES: &[(f64, u8, u8)] = &[
    (62.5, 7, 5),  // the public USA/Canada preset
    (500.0, 7, 5), // the home bench
    (125.0, 9, 5),
    (250.0, 11, 5),
    (125.0, 12, 8), // the slowest legal setting, where low-data-rate optimisation turns on
];

/// Every packet size the radio route puts on the air: the two-byte header of an empty packet up to
/// the 176-byte companion frame ceiling, including the sizes the plan names explicitly (42, 65, 93,
/// 121, 172).
#[test]
fn the_two_copies_of_the_airtime_formula_agree_everywhere() {
    let sizes = (2..=176).collect::<Vec<_>>();
    for &(bw_khz, sf, cr) in TUPLES {
        let m = Modulation { bw_khz, sf, cr };
        for bytes in sizes.clone() {
            let shared = airtime_ms(m, bytes);
            let shipping = time_on_air_ms(bw_khz, sf, cr, bytes);
            assert!(
                (shared - shipping).abs() < 0.1,
                "at {bw_khz} kHz SF{sf} CR{cr} and {bytes} B the two formulas disagree: \
                 ldmux-mesh says {shared} ms, ldmux-companion says {shipping} ms. They are one \
                 number; fix whichever changed."
            );
        }
    }
}

/// The two figures `docs/AIRTIME.md` and `docs/RADIO-AUDIT-REVIEW.md` §6.1 quote as measured at the
/// public preset. Both crates must reproduce them, so a reader of either page is reading the code.
#[test]
fn the_recorded_public_preset_figures_hold_in_both_crates() {
    let m = Modulation::PUBLIC_PRESET;
    for (bytes, recorded) in [(42usize, 223.7f64), (172, 602.6)] {
        let shared = airtime_ms(m, bytes);
        let shipping = time_on_air_ms(m.bw_khz, m.sf, m.cr, bytes);
        assert!(
            (shared - recorded).abs() < 0.2,
            "ldmux-mesh prices {bytes} B at {shared} ms; the measured figure is {recorded} ms"
        );
        assert!(
            (shipping - recorded).abs() < 0.2,
            "ldmux-companion prices {bytes} B at {shipping} ms; the measured figure is {recorded} ms"
        );
    }
}

/// Airtime never falls as a packet grows, and it is not strictly increasing: the payload is sent as
/// a whole number of symbols, so two sizes can round to the same count and cost the same air.
///
/// The weaker property is the true one and is the one a budget may rely on. Asserting strict
/// increase would be asserting something the radio does not do, and the first small size to collide
/// would read as a regression.
#[test]
fn a_longer_packet_never_costs_less_air() {
    let m = Modulation::PUBLIC_PRESET;
    let mut previous = 0.0;
    for bytes in 2..=176 {
        let now = airtime_ms(m, bytes);
        assert!(
            now >= previous,
            "{bytes} B priced at {now} ms, less than the {previous} ms before it"
        );
        previous = now;
    }
    // And it does grow overall: the ceiling costs more than a fifth as many bytes.
    assert!(airtime_ms(m, 176) > 2.0 * airtime_ms(m, 42));
}

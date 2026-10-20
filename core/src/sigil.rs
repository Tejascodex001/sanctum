//! Sigils: deterministic generative art derived from a chat's key fingerprint.
//!
//! The same fingerprint always yields the same sigil, so two friends verify
//! each other by checking their sigils match. The UI renders [`Sigil`]; this
//! module only decides *what* to draw.

use crate::identity::Fingerprint;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Hsl {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Form {
    Rings,
    Petals,
    Shards,
    Waves,
    Lattice,
    Spiral,
}

const FORMS: [Form; 6] = [Form::Rings, Form::Petals, Form::Shards, Form::Waves, Form::Lattice, Form::Spiral];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sigil {
    /// Chat accent colour, also used for bubbles and the deck card edge.
    pub accent: Hsl,
    /// Two supporting colours for the generative layers.
    pub palette: [Hsl; 2],
    pub form: Form,
    /// Rotational symmetry, 3..=9.
    pub symmetry: u8,
    /// Number of stacked layers, 2..=5.
    pub layers: u8,
    /// Starting rotation in degrees.
    pub rotation: f32,
    /// Seed for fine detail (noise, jitter) so the renderer stays deterministic.
    pub seed: u64,
}

impl Sigil {
    pub fn from_fingerprint(fp: &Fingerprint) -> Self {
        let unit = |b: u8| b as f32 / 255.0;
        let base_hue = u16::from_le_bytes([fp[0], fp[1]]) as f32 / 65535.0 * 360.0;
        // Supporting hues sit at an analogous or split-complementary offset so
        // every palette is harmonious rather than random.
        let offsets = [30.0, 150.0, 180.0, 210.0];
        let off = offsets[(fp[2] % 4) as usize];
        let hue = |h: f32| h.rem_euclid(360.0);

        Self {
            accent: Hsl { h: base_hue, s: 0.65 + unit(fp[3]) * 0.25, l: 0.55 + unit(fp[4]) * 0.1 },
            palette: [
                Hsl { h: hue(base_hue + off), s: 0.5 + unit(fp[5]) * 0.3, l: 0.35 + unit(fp[6]) * 0.2 },
                Hsl { h: hue(base_hue - off / 2.0), s: 0.4 + unit(fp[7]) * 0.3, l: 0.2 + unit(fp[8]) * 0.15 },
            ],
            form: FORMS[(fp[9] as usize) % FORMS.len()],
            symmetry: 3 + fp[10] % 7,
            layers: 2 + fp[11] % 4,
            rotation: unit(fp[12]) * 360.0,
            seed: u64::from_le_bytes(fp[16..24].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn deterministic() {
        let fp = Identity::generate().fingerprint();
        assert_eq!(Sigil::from_fingerprint(&fp), Sigil::from_fingerprint(&fp));
    }

    #[test]
    fn values_in_range_and_distinct() {
        let a = Sigil::from_fingerprint(&Identity::generate().fingerprint());
        let b = Sigil::from_fingerprint(&Identity::generate().fingerprint());
        for s in [&a, &b] {
            assert!((3..=9).contains(&s.symmetry));
            assert!((2..=5).contains(&s.layers));
            assert!((0.0..360.0).contains(&s.accent.h));
        }
        assert_ne!(a, b);
    }
}

//! Sigils: deterministic generative emblems derived from a key fingerprint.
//!
//! The same fingerprint always yields the same sigil, so two friends verify
//! each other by checking their sigils match. This module decides *what* to
//! draw; the UI turns it into crisp vector art.
//!
//! Every byte of the fingerprint that matters for recognition (family,
//! palette, symmetry, complexity) comes from a different part of the hash,
//! so two different keys almost never share a look.

use crate::identity::Fingerprint;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Hsl {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

/// The structural family of the emblem. Each looks fundamentally different.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Form {
    /// Spirograph curves layered at different radii.
    Rosette,
    /// Star-map rings of points joined by straight lines.
    Constellation,
    /// Mirrored kaleidoscope of flat polygons.
    Shards,
    /// Concentric broken rings of varying weight.
    Orbits,
    /// Twisted concentric polygons that form a moiré.
    Lattice,
    /// A mirrored rune drawn on a grid.
    Glyph,
}

const FORMS: [Form; 6] = [
    Form::Rosette,
    Form::Constellation,
    Form::Shards,
    Form::Orbits,
    Form::Lattice,
    Form::Glyph,
];

/// Colour harmony used to derive the palette from the base hue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scheme {
    Mono,
    Complement,
    Triad,
    Analogous,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sigil {
    /// The chat's signature colour: bright, used for highlights and UI tint.
    pub accent: Hsl,
    /// Three supporting colours for the emblem's layers.
    pub palette: [Hsl; 3],
    /// Deep, dark tint behind the emblem.
    pub background: Hsl,
    pub form: Form,
    pub scheme: Scheme,
    /// Rotational symmetry, 3..=12.
    pub symmetry: u8,
    /// Number of layers or rings, 2..=6.
    pub layers: u8,
    /// Starting rotation in degrees.
    pub rotation: f32,
    /// 0..1: how busy the emblem is.
    pub density: f32,
    /// 0..1: hairline to bold strokes.
    pub weight: f32,
    /// Seed for the renderer's fine detail, so rendering stays deterministic.
    pub seed: u64,
}

impl Sigil {
    pub fn from_fingerprint(fp: &Fingerprint) -> Self {
        let unit = |b: u8| b as f32 / 255.0;
        let hue = |h: f32| h.rem_euclid(360.0);
        let base = u16::from_le_bytes([fp[0], fp[1]]) as f32 / 65536.0 * 360.0;

        let scheme = [
            Scheme::Mono,
            Scheme::Complement,
            Scheme::Triad,
            Scheme::Analogous,
        ][(fp[2] % 4) as usize];
        let offsets: [f32; 3] = match scheme {
            Scheme::Mono => [0.0, 0.0, 0.0],
            Scheme::Complement => [180.0, 0.0, 180.0],
            Scheme::Triad => [120.0, 240.0, 0.0],
            Scheme::Analogous => [30.0, -30.0, 60.0],
        };
        let palette = [0, 1, 2].map(|i| Hsl {
            h: hue(base + offsets[i]),
            s: 0.45 + unit(fp[5 + i]) * 0.45,
            // Spread lightness so even a mono palette has clear contrast.
            l: [0.62, 0.45, 0.75][i] + (unit(fp[8 + i]) - 0.5) * 0.1,
        });

        Self {
            accent: Hsl {
                h: base,
                s: 0.7 + unit(fp[3]) * 0.25,
                l: 0.62 + unit(fp[4]) * 0.08,
            },
            palette,
            background: Hsl {
                h: hue(base + 15.0),
                s: 0.3 + unit(fp[11]) * 0.3,
                l: 0.07 + unit(fp[12]) * 0.04,
            },
            form: FORMS[(fp[13] as usize) % FORMS.len()],
            scheme,
            symmetry: 3 + fp[14] % 10,
            layers: 2 + fp[15] % 5,
            rotation: unit(fp[16]) * 360.0,
            density: unit(fp[17]),
            weight: unit(fp[18]),
            seed: u64::from_le_bytes(fp[24..32].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use std::collections::HashSet;

    fn random_sigil() -> Sigil {
        Sigil::from_fingerprint(&Identity::generate().fingerprint())
    }

    #[test]
    fn deterministic() {
        let fp = Identity::generate().fingerprint();
        assert_eq!(Sigil::from_fingerprint(&fp), Sigil::from_fingerprint(&fp));
    }

    #[test]
    fn values_in_range() {
        for _ in 0..200 {
            let s = random_sigil();
            assert!((3..=12).contains(&s.symmetry));
            assert!((2..=6).contains(&s.layers));
            assert!((0.0..360.0).contains(&s.accent.h));
            assert!((0.0..=1.0).contains(&s.density) && (0.0..=1.0).contains(&s.weight));
            for c in s.palette.iter().chain([&s.accent, &s.background]) {
                assert!((0.0..=1.0).contains(&c.s) && (0.0..=1.0).contains(&c.l));
            }
        }
    }

    #[test]
    fn sigils_vary_widely() {
        let sigils: Vec<_> = (0..300).map(|_| random_sigil()).collect();
        let forms: HashSet<_> = sigils.iter().map(|s| s.form as u8).collect();
        let schemes: HashSet<_> = sigils.iter().map(|s| s.scheme as u8).collect();
        let symmetries: HashSet<_> = sigils.iter().map(|s| s.symmetry).collect();
        assert_eq!(forms.len(), 6);
        assert_eq!(schemes.len(), 4);
        assert_eq!(symmetries.len(), 10);
        // Accent hues should cover the colour wheel, not cluster.
        let sextants: HashSet<_> = sigils.iter().map(|s| (s.accent.h / 60.0) as u8).collect();
        assert_eq!(sextants.len(), 6);
    }
}

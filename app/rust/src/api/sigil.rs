use sanctum_core::identity::{fingerprint, pair_fingerprint};
use sanctum_core::sigil::{Form, Hsl, Sigil};

pub struct HslColor {
    pub h: f64,
    pub s: f64,
    pub l: f64,
}

pub enum SigilForm {
    Rosette,
    Constellation,
    Shards,
    Orbits,
    Lattice,
    Glyph,
}

pub struct SigilSpec {
    pub accent: HslColor,
    /// Always three colours.
    pub palette: Vec<HslColor>,
    pub background: HslColor,
    pub form: SigilForm,
    pub symmetry: u8,
    pub layers: u8,
    pub rotation: f64,
    pub density: f64,
    pub weight: f64,
    /// Truncated to 32 bits so it fits Dart's `Random` seed.
    pub seed: u32,
}

/// The sigil for one person (shown on the lock screen and invite QR).
pub fn sigil_for_key(public_key: Vec<u8>) -> anyhow::Result<SigilSpec> {
    Ok(Sigil::from_fingerprint(&fingerprint(&key(public_key)?)).into())
}

/// The sigil for a 1:1 chat; both people get the same one.
pub fn sigil_for_pair(a: Vec<u8>, b: Vec<u8>) -> anyhow::Result<SigilSpec> {
    Ok(Sigil::from_fingerprint(&pair_fingerprint(&key(a)?, &key(b)?)).into())
}

/// A one-off sigil, e.g. for a new temp chat.
pub fn sigil_random() -> SigilSpec {
    Sigil::from_fingerprint(&rand::random()).into()
}

fn key(bytes: Vec<u8>) -> anyhow::Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))
}

impl From<Hsl> for HslColor {
    fn from(c: Hsl) -> Self {
        Self {
            h: c.h.into(),
            s: c.s.into(),
            l: c.l.into(),
        }
    }
}

impl From<Sigil> for SigilSpec {
    fn from(s: Sigil) -> Self {
        Self {
            accent: s.accent.into(),
            palette: s.palette.into_iter().map(Into::into).collect(),
            background: s.background.into(),
            form: match s.form {
                Form::Rosette => SigilForm::Rosette,
                Form::Constellation => SigilForm::Constellation,
                Form::Shards => SigilForm::Shards,
                Form::Orbits => SigilForm::Orbits,
                Form::Lattice => SigilForm::Lattice,
                Form::Glyph => SigilForm::Glyph,
            },
            symmetry: s.symmetry,
            layers: s.layers,
            rotation: s.rotation.into(),
            density: s.density.into(),
            weight: s.weight.into(),
            seed: s.seed as u32,
        }
    }
}

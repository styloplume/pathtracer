//! Saving renders: color in PNG / Radiance HDR / PFM, an optional denoised copy,
//! and AOV images (albedo, normal, depth), all named after one base path.

use crate::denoise::denoise;
use crate::image::{self, Encoder, pack_unorm};
use crate::math::Vec3;
use crate::renderer::{Pixel, resolve_values};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    /// 8-bit sRGB after exposure and tone mapping.
    Png,
    /// Radiance RGBE, linear scene radiance.
    Hdr,
    /// Portable float map, linear scene radiance.
    Pfm,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "hdr" | "rgbe" => Some(Format::Hdr),
            "pfm" => Some(Format::Pfm),
            _ => None,
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Hdr => "hdr",
            Format::Pfm => "pfm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Aov {
    Albedo,
    Normal,
    Depth,
}

impl Aov {
    pub fn parse(s: &str) -> Option<Aov> {
        match s.to_ascii_lowercase().as_str() {
            "albedo" => Some(Aov::Albedo),
            "normal" | "normals" => Some(Aov::Normal),
            "depth" | "z" => Some(Aov::Depth),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Aov::Albedo => "albedo",
            Aov::Normal => "normal",
            Aov::Depth => "depth",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputSettings {
    /// Output path without extension; `{scene}` and `{time}` are substituted.
    /// `None` uses the front end's default.
    pub path: Option<String>,
    pub formats: Vec<Format>,
    /// Window mode: save automatically whenever a render finishes.
    pub save_on_finish: bool,
    /// Also write a denoised image (`<path>_denoised.<ext>`).
    pub denoise: bool,
    /// AOV images to write (`<path>_<aov>.<ext>`).
    pub aovs: Vec<Aov>,
}

impl Default for OutputSettings {
    fn default() -> Self {
        OutputSettings {
            path: None,
            formats: vec![Format::Png],
            save_on_finish: false,
            denoise: false,
            aovs: Vec::new(),
        }
    }
}

impl OutputSettings {
    /// Applies a command-line `-o FILE`: a known extension selects the single format.
    pub fn set_output_arg(&mut self, file: &Path) {
        match file.extension().and_then(|e| Format::parse(&e.to_string_lossy())) {
            Some(f) => {
                self.formats = vec![f];
                self.path = Some(file.with_extension("").to_string_lossy().into_owned());
            }
            None => self.path = Some(file.to_string_lossy().into_owned()),
        }
    }

    /// Base path (no extension) with placeholders filled in.
    pub fn base_path(&self, scene_stem: &str, default: &str) -> PathBuf {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let template = self.path.as_deref().unwrap_or(default);
        PathBuf::from(template.replace("{scene}", scene_stem).replace("{time}", &secs.to_string()))
    }
}

/// What a buffer holds, which decides how it is mapped to 8-bit and HDR files.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Radiance,
    Albedo,
    Normal,
    Depth,
}

/// A (possibly still previewing) accumulation buffer to save.
pub struct Snapshot<'a> {
    pub pixels: &'a [Pixel],
    pub w: usize,
    pub h: usize,
    /// Preview block size; pixels not traced yet take their anchor's value.
    pub block: usize,
}

/// Writes all configured outputs; returns the files written.
pub fn save(
    snap: &Snapshot,
    settings: &OutputSettings,
    base: &Path,
    enc: &Encoder,
    threads: usize,
) -> Result<Vec<PathBuf>, String> {
    let (w, h) = (snap.w, snap.h);
    let values = |f: fn(&Pixel) -> Vec3| resolve_values(snap.pixels, w, h, snap.block, f);
    let mut images: Vec<(String, Kind, Vec<Vec3>)> = vec![(String::new(), Kind::Radiance, values(Pixel::mean))];
    if settings.denoise {
        if snap.block == 1 && snap.pixels.iter().all(|p| p.n > 0) {
            images.push(("_denoised".into(), Kind::Radiance, denoise(snap.pixels, w, h, threads)));
        } else {
            eprintln!("warning: not denoising a render that has not reached full resolution");
        }
    }
    for &aov in &settings.aovs {
        let (kind, data) = match aov {
            Aov::Albedo => (Kind::Albedo, values(Pixel::albedo)),
            Aov::Normal => (Kind::Normal, values(Pixel::normal)),
            Aov::Depth => (Kind::Depth, values(|p| Vec3::splat(p.depth()))),
        };
        images.push((format!("_{}", aov.name()), kind, data));
    }

    let mut written = Vec::new();
    for (suffix, kind, data) in &images {
        for &format in &settings.formats {
            let path = PathBuf::from(format!("{}{suffix}.{}", base.display(), format.ext()));
            write_image(&path, format, w, h, data, *kind, enc)
                .map_err(|e| format!("cannot write '{}': {e}", path.display()))?;
            println!("saved {}", path.display());
            written.push(path);
        }
    }
    Ok(written)
}

fn write_image(
    path: &Path,
    format: Format,
    w: usize,
    h: usize,
    data: &[Vec3],
    kind: Kind,
    enc: &Encoder,
) -> std::io::Result<()> {
    let normal_to_unit = |c: &Vec3| *c * 0.5 + Vec3::splat(0.5);
    match format {
        Format::Png => {
            let packed: Vec<u32> = match kind {
                Kind::Radiance => data.iter().map(|&c| enc.encode(c)).collect(),
                Kind::Albedo => data.iter().map(|&c| enc.encode_srgb(c)).collect(),
                Kind::Normal => data.iter().map(|c| pack_unorm(normal_to_unit(c))).collect(),
                Kind::Depth => {
                    let max = data.iter().map(|c| c.x).fold(0.0f32, f32::max).max(1e-6);
                    data.iter()
                        .map(|c| if c.x > 0.0 { pack_unorm(Vec3::splat(1.0 - 0.9 * c.x / max)) } else { 0 })
                        .collect()
                }
            };
            image::write_png(path, w, h, &image::packed_to_rgb(&packed))
        }
        Format::Hdr => {
            // RGBE has no sign bit: store normals remapped to [0, 1].
            if kind == Kind::Normal {
                image::write_hdr(path, w, h, &data.iter().map(normal_to_unit).collect::<Vec<_>>())
            } else {
                image::write_hdr(path, w, h, data)
            }
        }
        Format::Pfm => image::write_pfm(path, w, h, data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_arg_selects_format() {
        let mut o = OutputSettings::default();
        o.set_output_arg(Path::new("out/img.hdr"));
        assert_eq!(o.formats, vec![Format::Hdr]);
        assert_eq!(o.base_path("x", "unused"), PathBuf::from("out/img"));
        let mut o = OutputSettings::default();
        o.set_output_arg(Path::new("out/{scene}_final"));
        assert_eq!(o.formats, vec![Format::Png]);
        assert_eq!(o.base_path("cornell", "unused"), PathBuf::from("out/cornell_final"));
    }
}

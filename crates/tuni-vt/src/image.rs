//! Inline images: the Kitty graphics protocol, as far as drawing them goes.
//!
//! The library stores the images and works out where they belong; what it does
//! not do is put pixels on a surface, so this is the half of the protocol that
//! has to cross the facade. Two things go up: a list of [`Placement`]s, which
//! is geometry alone and is recomputed every frame because scrolling moves an
//! image without touching the storage, and [`Pixels`], which is the image
//! itself and is fetched only when a texture cache misses.
//!
//! Virtual placements — kitty's unicode placeholder, the form tmux passes
//! through — are stored and iterated but carry no viewport position, and the C
//! API exposes no way to work one out. They are skipped rather than guessed at.

use libghostty_vt::alloc::{Allocator, Bytes};
use libghostty_vt::kitty::graphics::{
    DecodePng, DecodedImage, ImageFormat, PlacementIterator, set_png_decoder,
};
use libghostty_vt::terminal::Terminal as VtTerminal;

use crate::Result;

/// How much image data one terminal may hold. Ghostty's own limit is 320 MB;
/// this is a workspace of terminals rather than one, and an image storage is
/// per screen, so the ceiling is lower. It bounds nothing that is not drawn:
/// the storage evicts its oldest images to stay under it.
pub(crate) const STORAGE_LIMIT: u64 = 64 * 1024 * 1024;

/// The largest PNG the decoder will expand. A 4-byte-per-pixel bomb decodes to
/// far more than it arrives as, and the storage limit above is only checked
/// once the expansion is already in memory.
const MAX_DECODED: usize = 96 * 1024 * 1024;

/// Which image a placement draws, and which version of it.
///
/// The generation is what makes this a cache key: an image retransmitted under
/// the same id with the same dimensions is a different picture, and only the
/// generation says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub id: u32,
    pub generation: u64,
}

/// One image on screen: which image, where it goes, and which part of it.
///
/// Coordinates are the viewport's own, in cells, and `row` is signed because a
/// placement whose top has scrolled off the screen keeps its origin above it.
/// The offsets are pixels within the origin cell. Clipping to the widget is the
/// caller's, as upstream documents.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub image: ImageKey,
    pub col: i32,
    pub row: i32,
    pub x_offset: u32,
    pub y_offset: u32,
    /// Size the image is drawn at, in pixels, after the placement's own columns
    /// and rows and its aspect ratio have been applied.
    pub width: u32,
    pub height: u32,
    /// The part of the image that is drawn, in image pixels.
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    /// Stacking order. Negative is under the text, and far enough negative is
    /// under the cell backgrounds as well; zero and up is over everything.
    pub z: i32,
}

impl Placement {
    /// Where this placement sits in the three layers the protocol defines.
    #[must_use]
    pub fn layer(&self) -> Layer {
        if self.z < i32::MIN / 2 {
            Layer::BelowBackground
        } else if self.z < 0 {
            Layer::BelowText
        } else {
            Layer::AboveText
        }
    }
}

/// Where a placement is drawn relative to the cells it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    BelowBackground,
    BelowText,
    AboveText,
}

/// An image's pixels, normalized to 8-bit RGBA because that is what a texture
/// wants and the storage keeps gray and RGB as they arrived.
#[derive(Clone, Debug)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Collect every visible placement, nearest layer last.
///
/// Returns an empty list when the storage has never held an image, which is the
/// case for all but a handful of terminals and costs one call to find out.
pub(crate) fn placements(
    term: &VtTerminal<'static, 'static>,
    out: &mut Vec<Placement>,
) -> Result<()> {
    out.clear();
    let graphics = term.kitty_graphics()?;
    if graphics.generation()? == 0 {
        return Ok(());
    }

    let mut iterator = PlacementIterator::new()?;
    let mut iteration = iterator.update(&graphics)?;
    while let Some(placement) = iteration.next() {
        let id = placement.image_id()?;
        let Some(image) = graphics.image(id) else {
            continue;
        };
        let info = placement.placement_render_info(&image, term)?;
        // False for a placement scrolled entirely out of the viewport, and for
        // a virtual one, whose position only the unicode placeholder cells know.
        if !info.viewport_visible {
            continue;
        }
        out.push(Placement {
            image: ImageKey {
                id,
                generation: image.generation()?,
            },
            col: info.viewport_col,
            row: info.viewport_row,
            x_offset: placement.x_offset()?,
            y_offset: placement.y_offset()?,
            width: info.pixel_width,
            height: info.pixel_height,
            source_x: info.source_x,
            source_y: info.source_y,
            source_width: info.source_width,
            source_height: info.source_height,
            z: placement.z()?,
        });
    }

    // Kitty orders equal z by image id, and the layers stack in z order.
    out.sort_by_key(|placement| (placement.z, placement.image.id));
    Ok(())
}

/// The pixels of one stored image, as RGBA.
///
/// Returns `None` when the id names nothing, which happens when a placement is
/// read a frame after its image was deleted.
pub(crate) fn pixels(term: &VtTerminal<'static, 'static>, id: u32) -> Result<Option<Pixels>> {
    let graphics = term.kitty_graphics()?;
    let Some(image) = graphics.image(id) else {
        return Ok(None);
    };
    let width = image.width()?;
    let height = image.height()?;
    let data = image.data()?;

    // Stored images are always decompressed and never still PNG — loading
    // inflates and decodes before an image is completed — so this is the whole
    // set of formats the data can be in.
    let rgba = match image.format()? {
        ImageFormat::Rgba => data.to_vec(),
        ImageFormat::Rgb => expand(data, 3, |chunk, out| {
            out.extend_from_slice(chunk);
            out.push(0xff);
        }),
        ImageFormat::GrayAlpha => expand(data, 2, |chunk, out| {
            out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
        }),
        ImageFormat::Gray => expand(data, 1, |chunk, out| {
            out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], 0xff]);
        }),
        // PNG is decoded during loading, so a stored image is never one. If
        // upstream ever changes that, drawing nothing beats drawing noise.
        ImageFormat::Png => return Ok(None),
        _ => return Ok(None),
    };

    // A truncated image would otherwise be read past its end by whatever builds
    // the texture, which trusts the dimensions.
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if rgba.len() < expected {
        return Ok(None);
    }

    Ok(Some(Pixels {
        width,
        height,
        rgba,
    }))
}

fn expand(data: &[u8], stride: usize, mut per_pixel: impl FnMut(&[u8], &mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / stride * 4);
    for chunk in data.chunks_exact(stride) {
        per_pixel(chunk, &mut out);
    }
    out
}

/// Install the PNG decoder for this thread, once.
///
/// The library holds one decoder per thread and calls it while loading an image
/// transmitted as `f=100`. Without one, PNG transmissions are refused — which
/// is most of them, since `f=100` is what every image tool sends by default.
pub(crate) fn install_png_decoder() {
    thread_local! {
        static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    INSTALLED.with(|installed| {
        if installed.replace(true) {
            return;
        }
        let _ = set_png_decoder(Some(Box::new(Decoder::default())));
    });
}

/// A PNG decoder over the `png` crate.
///
/// Upstream ships one of these behind a feature flag; this one is here because
/// the buffer it decodes into has to be sized rather than merely reserved, and
/// because a terminal accepts images from whatever is on the other end of the
/// PTY, so a decode has to have a ceiling.
#[derive(Default)]
struct Decoder {
    /// Reused between images: a program drawing a plot at ten frames a second
    /// otherwise allocates a full bitmap ten times a second.
    buf: Vec<u8>,
}

impl DecodePng for Decoder {
    fn decode_png<'alloc>(
        &mut self,
        alloc: &'alloc Allocator<'_>,
        data: &[u8],
    ) -> Option<DecodedImage<'alloc>> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
        // Palette and sub-byte gray expand to whole bytes, 16-bit channels drop
        // to 8, and an alpha channel is added where there is none. What is left
        // to handle by hand is gray, which the crate will not widen to RGB.
        decoder.set_transformations(
            png::Transformations::EXPAND
                | png::Transformations::STRIP_16
                | png::Transformations::ALPHA,
        );

        let mut reader = decoder.read_info().ok()?;
        let size = reader.output_buffer_size()?;
        if size > MAX_DECODED {
            return None;
        }
        self.buf.clear();
        self.buf.resize(size, 0);

        let info = reader.next_frame(&mut self.buf).ok()?;
        let frame = &self.buf[..info.buffer_size()];
        let rgba = match info.color_type {
            png::ColorType::Rgba => frame.to_vec(),
            png::ColorType::GrayscaleAlpha => expand(frame, 2, |chunk, out| {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }),
            png::ColorType::Rgb => expand(frame, 3, |chunk, out| {
                out.extend_from_slice(chunk);
                out.push(0xff);
            }),
            png::ColorType::Grayscale => expand(frame, 1, |chunk, out| {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], 0xff]);
            }),
            png::ColorType::Indexed => return None,
        };

        // The buffer has to come from the allocator the library handed in: it
        // takes ownership of it and frees it with that same allocator.
        let mut bytes = Bytes::new_with_alloc(alloc, rgba.len()).ok()?;
        bytes.copy_from_slice(&rgba);
        Some(DecodedImage {
            width: info.width,
            height: info.height,
            data: bytes,
        })
    }
}

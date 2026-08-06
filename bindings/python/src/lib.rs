//! Python bindings for Photonic Zero.
//!
//! The API here is Pythonic rather than a transliteration of the Rust: bytes
//! in, bytes out, keyword arguments, exceptions instead of `Result`, and the
//! GIL released around the expensive decode.

use pyo3::exceptions::{PyBufferError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

use pz_core::render::{render, RenderOptions};
use pz_core::{
    Decoder as CoreDecoder, Encoder as CoreEncoder, EncoderConfig, Progress, PzError, RgbView,
};

pyo3::create_exception!(
    photonic_zero,
    PhotonicZeroError,
    pyo3::exceptions::PyException,
    "Raised when Photonic Zero cannot encode or decode."
);

fn to_py(error: PzError) -> PyErr {
    PhotonicZeroError::new_err(error.to_string())
}

/// Build render options, validating the ink before anything is drawn.
///
/// `ink` is `"#1a2b3c"` or an `(r, g, b)` tuple. A colour too close to the
/// page cannot be demodulated, and failing here beats handing back frames that
/// never lock on.
fn options_for(
    module_px: usize,
    quiet_zone: usize,
    ink: Option<&Bound<'_, PyAny>>,
) -> PyResult<RenderOptions> {
    let ink = match ink {
        None => None,
        Some(value) => Some(parse_ink(value)?),
    };
    let options = RenderOptions {
        module_px: module_px.max(1),
        quiet_zone,
        background: [255, 255, 255],
        ink,
    };
    if !options.contrast_ok() {
        return Err(PyValueError::new_err(
            "ink is too light to demodulate against a white page; use a deeper colour",
        ));
    }
    Ok(options)
}

fn parse_ink(value: &Bound<'_, PyAny>) -> PyResult<[u8; 3]> {
    if let Ok(text) = value.extract::<String>() {
        let hex = text.trim().trim_start_matches('#');
        let hex = if hex.len() == 3 {
            hex.chars().flat_map(|c| [c, c]).collect::<String>()
        } else {
            hex.to_owned()
        };
        if hex.len() != 6 {
            return Err(PyValueError::new_err(format!(
                "ink is not a colour: {text:?}"
            )));
        }
        let mut rgb = [0u8; 3];
        for (i, slot) in rgb.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| PyValueError::new_err(format!("ink is not a colour: {text:?}")))?;
        }
        return Ok(rgb);
    }
    value
        .extract::<[u8; 3]>()
        .map_err(|_| PyValueError::new_err("ink must be a '#rrggbb' string or an (r, g, b) tuple"))
}

fn config_for(profile: &str) -> PyResult<EncoderConfig> {
    Ok(match profile {
        "balanced" | "default" => EncoderConfig::default(),
        "robust" => EncoderConfig::robust(),
        "fast" => EncoderConfig::fast(),
        "resilient" => EncoderConfig::resilient(),
        "mono" => EncoderConfig::mono(),
        other => {
            return Err(PyValueError::new_err(format!(
                "unknown profile {other:?}; expected 'balanced', 'robust', 'fast', \
                 'resilient' or 'mono'"
            )))
        }
    })
}

/// One frame of a transmission.
#[pyclass(module = "photonic_zero", frozen)]
pub struct Frame {
    inner: pz_core::Frame,
}

#[pymethods]
impl Frame {
    /// Cells per side.
    #[getter]
    fn modules(&self) -> usize {
        self.inner.modules()
    }

    /// The frame index this frame carries.
    #[getter]
    fn index(&self) -> u32 {
        self.inner.index()
    }

    /// Colour codes, one byte per cell, row-major.
    #[getter]
    fn cells<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.cells())
    }

    /// Flat RGB triples, one per cell, row-major.
    fn rgb<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let mut out = Vec::with_capacity(self.inner.cells().len() * 3);
        for colour in self.inner.to_colors() {
            out.extend_from_slice(&colour);
        }
        PyBytes::new(py, &out)
    }

    fn __repr__(&self) -> String {
        format!(
            "<photonic_zero.Frame index={} modules={}>",
            self.inner.index(),
            self.inner.modules()
        )
    }
}

/// Splits a message into an endless stream of frames.
#[pyclass(module = "photonic_zero", frozen)]
pub struct Encoder {
    inner: CoreEncoder,
}

#[pymethods]
impl Encoder {
    /// Prepare a transmission.
    #[new]
    #[pyo3(signature = (payload, *, profile = "balanced", session_id = None))]
    fn new(payload: &[u8], profile: &str, session_id: Option<u16>) -> PyResult<Self> {
        let mut config = config_for(profile)?;
        if session_id.is_some() {
            config.session_id = session_id;
        }
        Ok(Self {
            inner: CoreEncoder::new(payload, config).map_err(to_py)?,
        })
    }

    /// Minimum frames a receiver needs under perfect conditions.
    #[getter]
    fn block_count(&self) -> usize {
        self.inner.block_count()
    }

    /// Session id stamped into every frame.
    #[getter]
    fn session_id(&self) -> u16 {
        self.inner.session_id()
    }

    /// Cells per side.
    #[getter]
    fn modules(&self) -> usize {
        self.inner.layout().modules()
    }

    /// Payload bytes carried per frame.
    #[getter]
    fn droplet_size(&self) -> usize {
        self.inner.profile().droplet_size()
    }

    /// Length of the original message.
    #[getter]
    fn payload_len(&self) -> usize {
        self.inner.payload_len()
    }

    /// Build one frame. Defined for every index; the stream never runs out.
    fn frame(&self, index: u32) -> PyResult<Frame> {
        Ok(Frame {
            inner: self.inner.frame(index).map_err(to_py)?,
        })
    }

    /// Render one frame as a complete PNG file.
    #[pyo3(signature = (index, *, module_px = 8, quiet_zone = 4, ink = None))]
    fn png<'py>(
        &self,
        py: Python<'py>,
        index: u32,
        module_px: usize,
        quiet_zone: usize,
        ink: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let frame = self.inner.frame(index).map_err(to_py)?;
        let image = render(&frame, &options_for(module_px, quiet_zone, ink)?);
        Ok(PyBytes::new(py, &pz_core::png::encode(&image)))
    }

    /// Render one frame as raw RGB, returning `(width, height, pixels)`.
    #[pyo3(signature = (index, *, module_px = 8, quiet_zone = 4, ink = None))]
    fn rgb<'py>(
        &self,
        py: Python<'py>,
        index: u32,
        module_px: usize,
        quiet_zone: usize,
        ink: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(usize, usize, Bound<'py, PyBytes>)> {
        let frame = self.inner.frame(index).map_err(to_py)?;
        let image = render(&frame, &options_for(module_px, quiet_zone, ink)?);
        Ok((image.width, image.height, PyBytes::new(py, &image.data)))
    }

    /// Estimated transfer time in seconds.
    #[pyo3(signature = (fps = 30.0, capture_ratio = 0.8))]
    fn estimated_seconds(&self, fps: f64, capture_ratio: f64) -> f64 {
        self.inner.estimated_seconds(fps, capture_ratio)
    }

    fn __repr__(&self) -> String {
        format!(
            "<photonic_zero.Encoder modules={} droplet={} blocks={} session=0x{:04X}>",
            self.inner.layout().modules(),
            self.inner.profile().droplet_size(),
            self.inner.block_count(),
            self.inner.session_id()
        )
    }
}

/// What offering an image to a decoder achieved.
#[pyclass(module = "photonic_zero", frozen, eq)]
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum ProgressKind {
    /// No frame was located.
    NotFound,
    /// A frame was seen but was unusable, or belonged to another session.
    Rejected,
    /// A droplet was absorbed; the message is still incomplete.
    Progressed,
    /// The message is complete and verified.
    Complete,
}

/// The outcome of one ingest.
#[pyclass(module = "photonic_zero", frozen, get_all)]
pub struct Status {
    /// Which of the four outcomes occurred.
    pub kind: ProgressKind,
    /// True when the message is complete.
    pub complete: bool,
    /// Source blocks recovered so far.
    pub recovered: usize,
    /// Source blocks in the message.
    pub total: usize,
    /// Fraction recovered, in `[0, 1]`.
    pub fraction: f64,
}

#[pymethods]
impl Status {
    fn __repr__(&self) -> String {
        format!(
            "<photonic_zero.Status {:?} {}/{} ({:.0}%)>",
            self.kind,
            self.recovered,
            self.total,
            self.fraction * 100.0
        )
    }

    fn __bool__(&self) -> bool {
        self.complete
    }
}

/// Accumulates frames until the message is complete.
#[pyclass(module = "photonic_zero")]
pub struct Decoder {
    inner: CoreDecoder,
}

#[pymethods]
impl Decoder {
    #[new]
    fn new() -> Self {
        Self {
            inner: CoreDecoder::new(),
        }
    }

    /// Offer a captured image as tightly packed RGB or RGBA bytes.
    ///
    /// `NotFound` and `Rejected` are routine: most frames of a hand-held
    /// camera are unusable, which is exactly why the code is rateless.
    #[pyo3(signature = (width, height, data, *, channels = 3))]
    fn ingest(
        &mut self,
        py: Python<'_>,
        width: usize,
        height: usize,
        data: &[u8],
        channels: usize,
    ) -> PyResult<Status> {
        if channels != 3 && channels != 4 {
            return Err(PyValueError::new_err("channels must be 3 or 4"));
        }
        if width == 0 || height == 0 {
            return Err(PyValueError::new_err("width and height must be non-zero"));
        }
        if data.len() < width * height * channels {
            return Err(PyBufferError::new_err(format!(
                "buffer holds {} bytes but {}x{}x{} needs {}",
                data.len(),
                width,
                height,
                channels,
                width * height * channels
            )));
        }

        // Decoding an image is slow enough that holding the GIL would be a
        // real defect in any threaded application.
        let progress = py.allow_threads(|| {
            let view = if channels == 4 {
                RgbView::rgba(width, height, data)
            } else {
                RgbView::rgb(width, height, data)
            }
            .ok_or(PzError::WrongLength)?;
            self.inner.ingest_image(&view)
        });

        Ok(Self::status(progress.map_err(to_py)?, &self.inner))
    }

    /// Offer a frame directly, bypassing the camera path.
    fn ingest_frame(&mut self, frame: &Frame) -> PyResult<Status> {
        let progress = self.inner.ingest_frame(&frame.inner).map_err(to_py)?;
        Ok(Self::status(progress, &self.inner))
    }

    /// Fraction of the message recovered, in `[0, 1]`.
    #[getter]
    fn progress(&self) -> f64 {
        self.inner.progress()
    }

    /// Images offered so far.
    #[getter]
    fn frames_seen(&self) -> usize {
        self.inner.frames_seen()
    }

    /// Frames that decoded and were absorbed.
    #[getter]
    fn frames_accepted(&self) -> usize {
        self.inner.frames_accepted()
    }

    /// Session being received, or `None` if none has been locked on to.
    #[getter]
    fn session_id(&self) -> Option<u16> {
        self.inner.session_id()
    }

    /// The completed message, or `None` if decoding has not finished.
    #[getter]
    fn result<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        self.inner.result().map(|bytes| PyBytes::new(py, bytes))
    }

    /// Forget the current session so a new transmission can be received.
    fn reset(&mut self) {
        self.inner.reset();
    }

    fn __repr__(&self) -> String {
        format!(
            "<photonic_zero.Decoder progress={:.0}% seen={} accepted={}>",
            self.inner.progress() * 100.0,
            self.inner.frames_seen(),
            self.inner.frames_accepted()
        )
    }
}

impl Decoder {
    fn status(progress: Progress, decoder: &CoreDecoder) -> Status {
        // Every field is read from the decoder rather than from the `Progress`
        // variant. Taking the counts from the variant meant a routine
        // `NotFound` reported 0/0 halfway through a transfer, and a `Complete`
        // reported 0/0 at the very moment the numbers mattered most.
        let kind = match progress {
            Progress::NotFound => ProgressKind::NotFound,
            Progress::Rejected => ProgressKind::Rejected,
            Progress::Progressed { .. } => ProgressKind::Progressed,
            Progress::Complete(_) => ProgressKind::Complete,
        };
        Status {
            kind,
            complete: kind == ProgressKind::Complete,
            recovered: decoder.recovered(),
            total: decoder.total(),
            fraction: decoder.progress(),
        }
    }
}

/// Convenience wrapper: build an encoder for `payload`.
#[pyfunction]
#[pyo3(signature = (payload, *, profile = "balanced", session_id = None))]
fn encode(payload: &[u8], profile: &str, session_id: Option<u16>) -> PyResult<Encoder> {
    Encoder::new(payload, profile, session_id)
}

/// Capacity of one profile, as a dictionary of the interesting numbers.
#[pyfunction]
fn profile_info(profile: &str) -> PyResult<(usize, usize, usize, usize)> {
    let config = config_for(profile)?;
    let layout = pz_core::Layout::new(config.grid);
    let plan =
        pz_core::FrameProfile::new(config.grid, config.mode, config.parity_code).map_err(to_py)?;
    Ok((
        layout.modules(),
        config.mode.bits_per_cell(),
        plan.data_cells(),
        plan.droplet_size(),
    ))
}

/// Photonic Zero: data over light, from any screen to any camera.
#[pymodule]
fn _photonic_zero(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add("PROTOCOL_VERSION", pz_core::PROTOCOL_VERSION)?;
    module.add(
        "PhotonicZeroError",
        module.py().get_type::<PhotonicZeroError>(),
    )?;
    module.add_class::<Encoder>()?;
    module.add_class::<Decoder>()?;
    module.add_class::<Frame>()?;
    module.add_class::<Status>()?;
    module.add_class::<ProgressKind>()?;
    module.add_function(wrap_pyfunction!(encode, module)?)?;
    module.add_function(wrap_pyfunction!(profile_info, module)?)?;
    Ok(())
}

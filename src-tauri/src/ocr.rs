//! Latin-script OCR via `ocrs` (pure-Rust, `rten` runtime). On first use the
//! detection + recognition models (~16 MB) are downloaded and cached to disk.
//!
//! Note: the default ocrs models are Latin-only — Chinese/CJK is not supported.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten::Model;

// The ocrs models are published by their author on Hugging Face. The old
// `ocrs-models.s3-accelerate.amazonaws.com` bucket was taken down (it answers
// 403 AccessDenied since 2026-10), which left every first use of an image
// stuck on a download that could never succeed.
const DET_URL: &str = "https://huggingface.co/robertknight/ocrs/resolve/main/text-detection-ssfbcj81.rten";
const REC_URL: &str = "https://huggingface.co/robertknight/ocrs/resolve/main/text-rec-checkpoint-s52qdbqt.rten";

async fn ensure_models(dir: &Path) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dir).context(trf!("无法创建模型文件夹", "could not create the models folder"))?;
    let det = dir.join("text-detection.rten");
    let rec = dir.join("text-recognition.rten");
    if !det.exists() {
        download(DET_URL, &det).await?;
    }
    if !rec.exists() {
        download(REC_URL, &rec).await?;
    }
    Ok((det, rec))
}

async fn download(url: &str, to: &Path) -> Result<()> {
    let bytes = reqwest::get(url)
        .await
        .with_context(|| trf!("下载 OCR 模型失败:{url}", "could not download the OCR model: {url}"))?
        .error_for_status()?
        .bytes()
        .await?;
    // Write beside the target and move it into place. Written straight to the
    // final name, a write cut short — a full disk, a quit — left a file that
    // exists, which is the only thing `ensure_models` asks: the download was
    // never retried and every load failed from then on.
    let tmp = to.with_extension("part");
    std::fs::write(&tmp, &bytes).context(trf!("无法写入模型文件", "could not write the model file"))?;
    std::fs::rename(&tmp, to).context(trf!("无法保存模型文件", "could not save the model file"))?;
    Ok(())
}

/// What the operating system's own OCR made of an image.
#[cfg(windows)]
enum SystemOcr {
    /// The engine ran and read the image.
    Text(String),
    /// The engine ran and found no text in the image. `chinese` says whether
    /// it was a Chinese recognizer — an English one cannot read a Chinese
    /// page, which is a different problem from an empty image.
    NoText { chinese: bool },
    /// No engine could be created, or the image could not be decoded.
    Unavailable,
}

/// Read an image with the operating system's own OCR (Windows only).
///
/// The outcome separates "the engine read nothing" from "no engine ran":
/// only the first says anything about the picture, and only the second tells
/// the user their system cannot read Chinese at all.
#[cfg(windows)]
fn system_ocr(path: &Path) -> SystemOcr {
    use windows::core::Result as WinResult;
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };

    /// A recognizer for a Chinese language when the system has one, else one
    /// for the user's own languages. The flag says which: Chinese is what
    /// the fallback below cannot do.
    fn engine() -> WinResult<(OcrEngine, bool)> {
        for lang in &OcrEngine::AvailableRecognizerLanguages()? {
            let chinese = lang
                .LanguageTag()?
                .to_string_lossy()
                .to_lowercase()
                .starts_with("zh");
            if chinese {
                if let Ok(found) = OcrEngine::TryCreateFromLanguage(&lang) {
                    return Ok((found, true));
                }
            }
        }
        Ok((OcrEngine::TryCreateFromUserProfileLanguages()?, false))
    }

    // WinRT activates its classes on a COM-initialized thread; a tokio worker
    // is not one. Already initialized (by whoever owns the thread) is fine —
    // and then the apartment is left as it was.
    let initialized =
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().is_ok();

    let run = || -> WinResult<(String, bool)> {
        let bytes = std::fs::read(path)?;
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(&bytes)?;
        writer.StoreAsync()?.join()?;
        writer.FlushAsync()?.join()?;
        drop(writer);
        stream.Seek(0)?;
        let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
        let bitmap = decoder
            .GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied)?
            .join()?;
        let (engine, chinese) = engine()?;
        Ok((
            engine.RecognizeAsync(&bitmap)?.join()?.Text()?.to_string(),
            chinese,
        ))
    };

    let out = match run() {
        Ok((text, _)) if !text.trim().is_empty() => SystemOcr::Text(text),
        Ok((_, chinese)) => SystemOcr::NoText { chinese },
        Err(e) => {
            eprintln!("[ocr] windows ocr did not run: {e}");
            SystemOcr::Unavailable
        }
    };
    if initialized {
        unsafe { CoUninitialize() };
    }
    out
}

/// Whether Windows has a Chinese OCR recognizer installed at all.
#[cfg(windows)]
fn has_chinese_recognizer() -> bool {
    use windows::Media::Ocr::OcrEngine;
    let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() else {
        return false;
    };
    // The list is a WinRT vector view, not a Vec — it iterates, it has no
    // `.iter()`.
    for lang in &langs {
        let chinese = lang
            .LanguageTag()
            .map(|t| t.to_string_lossy().to_lowercase().starts_with("zh"))
            .unwrap_or(false);
        if chinese {
            return true;
        }
    }
    false
}

/// What an OCR run produced, and what the user needs to know about it.
pub struct OcrOutcome {
    pub text: String,
    /// Set when the text came from the Latin-only fallback because this
    /// system cannot read Chinese. Chinese then comes out as gibberish, and
    /// the user deserves to be told why instead of guessing.
    pub notice: Option<String>,
}

/// Extract Latin text from an image file. Returns empty string if no text found.
pub async fn ocr_image(models_dir: PathBuf, image_path: String) -> Result<String> {
    Ok(ocr_outcome(models_dir, image_path).await?.text)
}

/// OCR an image, reporting how the text was obtained.
pub async fn ocr_outcome(models_dir: PathBuf, image_path: String) -> Result<OcrOutcome> {
    // The bundled ocrs models read Latin script only; a Chinese page through
    // them comes back as Latin-looking gibberish. Windows' own OCR reads
    // Chinese (and Latin), so it goes first where it exists. Any failure —
    // no recognizer installed, an old Windows, a decode error — is not fatal:
    // the Latin path below still runs, exactly as before.
    #[cfg(windows)]
    let chinese_available = {
        let path = Path::new(&image_path).to_path_buf();
        match tokio::task::spawn_blocking(move || system_ocr(&path)).await {
            // The engine read the image — that is the answer, whatever script
            // it is written in.
            Ok(SystemOcr::Text(text)) => return Ok(OcrOutcome { text, notice: None }),
            // The engine ran and found nothing. Whether the system has a
            // Chinese recognizer decides if the Latin fallback below can be
            // trusted with a Chinese page.
            Ok(SystemOcr::NoText { chinese }) => chinese || has_chinese_recognizer(),
            // No engine at all: ask the system what it has.
            Ok(SystemOcr::Unavailable) => has_chinese_recognizer(),
            Err(e) => {
                eprintln!("[ocr] windows ocr task failed: {e}");
                has_chinese_recognizer()
            }
        }
    };

    let (det, rec) = ensure_models(&models_dir).await?;

    // Model loading + inference is CPU-bound and synchronous.
    let text = tokio::task::spawn_blocking(move || -> Result<String> {
        // A model file that will not load is a cache to drop, not a permanent
        // state: the next run downloads it again rather than failing forever.
        let detection_model = Model::load_file(&det).map_err(|e| {
            let _ = std::fs::remove_file(&det);
            anyhow::anyhow!(trf!("加载检测模型失败(已清除缓存,下次会重新下载):{e}", "could not load the text-detection model (its cache was cleared; it downloads again next time): {e}"))
        })?;
        let recognition_model = Model::load_file(&rec).map_err(|e| {
            let _ = std::fs::remove_file(&rec);
            anyhow::anyhow!(trf!("加载识别模型失败(已清除缓存,下次会重新下载):{e}", "could not load the text-recognition model (its cache was cleared; it downloads again next time): {e}"))
        })?;

        let engine = OcrEngine::new(OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })?;

        let img = image::open(&image_path)
            .context(trf!("打开图片失败", "could not open the image"))?
            .into_rgb8();
        let (w, h) = img.dimensions();
        let source = ImageSource::from_bytes(img.as_raw(), (w, h))
            .map_err(|e| anyhow::anyhow!(trf!("图像预处理失败:{e:?}", "image preprocessing failed: {e:?}")))?;
        let input = engine.prepare_input(source)?;
        engine.get_text(&input)
    })
    .await
    .context(trf!("OCR 任务异常", "the OCR task failed"))??;

    // The Latin models read Latin script only. If this system has no Chinese
    // OCR, a Chinese page comes back as gibberish — and the user cannot tell
    // that from a bad picture. Say so, once, in their language.
    #[cfg(windows)]
    let notice = if !chinese_available {
        Some(trf!(
            "系统缺少中文 OCR 组件，已回退到仅支持拉丁文字的识别——中文图片可能显示为乱码。可在「设置 → 时间和语言 → 语言和区域」中安装中文（简体）光学字符识别组件。",
            "This system has no Chinese OCR component, so the Latin-only recogniser was used — Chinese text in the image may come out as gibberish. Installing the Chinese (Simplified) OCR language pack in Settings → Time & language fixes it."
        ))
    } else {
        None
    };
    #[cfg(not(windows))]
    let notice = None;

    Ok(OcrOutcome { text, notice })
}

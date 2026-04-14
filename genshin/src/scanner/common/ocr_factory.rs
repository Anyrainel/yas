use anyhow::{Context, Result};
use image::RgbImage;

use yas::ocr::ImageToText;

/// Create an OCR model for the specified backend.
///
/// Supported backends:
/// - `"ppocrv4"` / `"paddlev4"`: PaddleOCR v4 (11M, best for substats)
/// - `"ppocrv5"` / `"paddlev5"` / default: PaddleOCR v5 (16M, best for names/text)
///
/// All model weights are embedded at compile time via `include_bytes!`.
pub fn create_ocr_model(backend: &str) -> Result<Box<dyn ImageToText<RgbImage> + Send>> {
    match backend.to_lowercase().as_str() {
        "paddlev4" | "ppocrv4" => {
            let model_bytes = include_bytes!("models/ch_PP-OCRv4_rec_infer.onnx");
            let dict_str = include_str!("models/ppocr_keys_v1.txt");
            let mut dict_vec: Vec<String> = dict_str.lines().map(|l| l.trim().to_string()).collect();
            dict_vec.push(String::from(" "));
            let model = yas::ocr::PPOCRModel::new(model_bytes, dict_vec)
                .context("ONNX v4模型初始化失败，请确认onnxruntime.dll存在且版本正确\
                         / ONNX v4 model init failed — ensure onnxruntime.dll exists and is the correct version")?;
            Ok(Box::new(model))
        }
        _ => {
            // Default: PPOCRv5
            let model_bytes = include_bytes!("models/PP-OCRv5_mobile_rec.onnx");
            let dict_str = include_str!("models/ppocrv5_dict.txt");
            let mut dict_vec: Vec<String> = dict_str.lines().map(|l| l.trim().to_string()).collect();
            dict_vec.push(String::from(" "));
            let model = yas::ocr::PPOCRModel::new(model_bytes, dict_vec)
                .context("ONNX v5模型初始化失败，请确认onnxruntime.dll存在且版本正确\
                         / ONNX v5 model init failed — ensure onnxruntime.dll exists and is the correct version")?;
            Ok(Box::new(model))
        }
    }
}

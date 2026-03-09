use anyhow::Result;
use image::RgbImage;

use yas::ocr::ImageToText;

/// Create an OCR model for the specified backend.
///
/// Supported backends:
/// - `"ppocrv3"` / `"paddlev3"`: PaddleOCR v3 (11M, fast, slightly less accurate)
/// - `"ppocrv4"` / `"paddlev4"`: PaddleOCR v4 (11M, improved accuracy)
/// - `"ppocrv5"` / `"paddlev5"` / default: PaddleOCR v5 (16M, best accuracy)
///
/// All model weights are embedded at compile time via `include_bytes!`.
pub fn create_ocr_model(backend: &str) -> Result<Box<dyn ImageToText<RgbImage> + Send>> {
    match backend.to_lowercase().as_str() {
        "paddlev3" | "ppocrv3" => {
            let model_bytes = include_bytes!("models/ch_PP-OCRv3_rec_infer.onnx");
            let dict_str = include_str!("models/ppocr_keys_v1.txt");
            let mut dict_vec: Vec<String> = dict_str.lines().map(|l| l.trim().to_string()).collect();
            dict_vec.push(String::from(" "));
            let model = yas::ocr::PPOCRModel::new(model_bytes, dict_vec)?;
            Ok(Box::new(model))
        }
        "paddlev4" | "ppocrv4" => {
            // PPOCRv4 uses the same dictionary as v3
            let model_bytes = include_bytes!("models/ch_PP-OCRv4_rec_infer.onnx");
            let dict_str = include_str!("models/ppocr_keys_v1.txt");
            let mut dict_vec: Vec<String> = dict_str.lines().map(|l| l.trim().to_string()).collect();
            dict_vec.push(String::from(" "));
            let model = yas::ocr::PPOCRModel::new(model_bytes, dict_vec)?;
            Ok(Box::new(model))
        }
        _ => {
            // Default: PPOCRv5
            let model_bytes = include_bytes!("models/PP-OCRv5_mobile_rec.onnx");
            let dict_str = include_str!("models/ppocrv5_dict.txt");
            let mut dict_vec: Vec<String> = dict_str.lines().map(|l| l.trim().to_string()).collect();
            dict_vec.push(String::from(" "));
            let model = yas::ocr::PPOCRModel::new(model_bytes, dict_vec)?;
            Ok(Box::new(model))
        }
    }
}

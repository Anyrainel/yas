#[cfg(feature = "ort")]
use std::sync::Mutex;
use std::path::Path;
use std::time::Duration;
use anyhow::Result;
use image::{EncodableLayout, RgbImage};
#[cfg(feature = "tract_onnx")]
use tract_onnx::tract_hir::shapefactoid;
use crate::ocr::ImageToText;
use crate::ocr::paddle_paddle_model::preprocess::resize_img;
use crate::positioning::Shape3D;
#[cfg(feature = "ort")]
use ort::{session::{Session, builder::GraphOptimizationLevel}, value::Value};
#[cfg(feature = "tract_onnx")]
use tract_onnx::prelude::*;
#[cfg(feature = "tract_onnx")]
use tract_onnx::tract_hir::infer::InferenceOp;

#[cfg(feature = "tract_onnx")]
use super::preprocess::normalize_image_to_tensor;
#[cfg(feature = "ort")]
use super::preprocess::normalize_image_to_ndarray;

#[cfg(feature = "tract_onnx")]
type ModelType = RunnableModel<InferenceFact, Box<dyn InferenceOp>, Graph<InferenceFact, Box<dyn InferenceOp>>>;

pub struct PPOCRModel {
    index_to_word: Vec<String>,
    #[cfg(feature = "tract_onnx")]
    model: ModelType,
    #[cfg(feature = "ort")]
    model: Mutex<Session>,
}

fn parse_index_to_word(s: &str, use_whitespace: bool) -> Vec<String> {
    let mut result = Vec::new();
    for line in s.lines() {
        result.push(String::from(line));
    }
    if use_whitespace {
        result.push(String::from(" "));
    }
    result
}

impl PPOCRModel {
    pub fn new_from_file<P1, P2>(onnx_file: P1, words_file: P2) -> Result<PPOCRModel> where P1: AsRef<Path>, P2: AsRef<Path> {
        let words_str = std::fs::read_to_string(words_file)?;
        let index_to_word = parse_index_to_word(&words_str, true);

        #[cfg(feature = "ort")]
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .commit_from_file(onnx_file)?;

        #[cfg(feature = "tract_onnx")]
        let model = {
            let fact = InferenceFact::new().with_datum_type(DatumType::F32)
                .with_shape(shapefactoid!(_, 3, _, _));

            tract_onnx::onnx()
                .model_for_path(onnx_file)?
                .with_input_fact(0, fact)?
                // .into_optimized()?
                .into_runnable()?
        };

        Ok(Self {
            index_to_word,
            #[cfg(feature = "ort")]
            model: Mutex::new(session),
            #[cfg(feature = "tract_onnx")]
            model,
        })
    }

    pub fn new(onnx: &[u8], index_to_word: Vec<String>) -> Result<Self> {
        #[cfg(feature = "ort")]
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .commit_from_memory(onnx)?;

        #[cfg(feature = "tract_onnx")]
        let model = {
            let fact = InferenceFact::new().with_datum_type(DatumType::F32)
                .with_shape(shapefactoid!(_, 3, _, _));

            tract_onnx::onnx()
                .model_for_read(&mut onnx.as_bytes())?
                .with_input_fact(0, fact)?
                // .into_optimized()?
                .into_runnable()?
        };

        Ok(Self {
            index_to_word,
            #[cfg(feature = "ort")]
            model: Mutex::new(session),
            #[cfg(feature = "tract_onnx")]
            model,
        })
    }

    pub fn get_average_inference_time(&self) -> Option<Duration> {
        None
    }
}

impl ImageToText<RgbImage> for PPOCRModel {
    fn image_to_text(&self, image: &RgbImage, _is_preprocessed: bool) -> Result<String> {
        // log::info!("========== [OCR调试] 开始新的识别 ==========");
        // log::info!("[OCR调试] 原始图像尺寸: {}x{}", image.width(), image.height());
        // If image is already preprocessed (fixed size for model), skip resize.
        let resized_image = if _is_preprocessed {
            // Assume caller provided an image already sized to model input (H=48, W=320)
            image.clone()
        } else {
            resize_img(Shape3D::new(3, 48, 320), image)
        };
        // log::info!("[OCR调试] 缩放后图像尺寸: {}x{}", resized_image.width(), resized_image.height());

        #[cfg(feature = "ort")]
        let tensor = normalize_image_to_ndarray(&resized_image);
        #[cfg(feature = "tract_onnx")]
        let tensor = normalize_image_to_tensor(&resized_image);

        #[cfg(feature = "ort")]
        {
            // log::info!("[OCR调试] 张量形状: {:?}", tensor.shape());
            // let first_pixel = resized_image.get_pixel(0, 0);
            // log::info!("[OCR调试] 第一个像素 RGB: [{}, {}, {}]", first_pixel[0], first_pixel[1], first_pixel[2]);
            // // 打印张量前几个值
            // let mut first_vals = Vec::new();
            // for i in 0..5.min(tensor.shape()[3]) {
            //     first_vals.push(tensor[[0, 0, 0, i]]);
            // }
            // log::info!("[OCR调试] 张量前几个值 (C=0): {:?}", first_vals);
        }
        
        #[cfg(feature = "ort")]
        let tensor_value = Value::from_array(tensor)?;
        #[cfg(feature = "ort")]
        let mut model = self.model.lock().unwrap();
        #[cfg(feature = "ort")]
        let result = model.run(ort::inputs![tensor_value])?;
        #[cfg(feature = "tract_onnx")]
        let result = self.model.run(tvec!(tensor.into()))?;

        #[cfg(feature = "ort")]
        let (shape, data) = result[0].try_extract_tensor::<f32>()?;
        #[cfg(feature = "tract_onnx")]
        let arr = result[0].to_array_view::<f32>()?;

        #[cfg(feature = "ort")]
        let shape_dims = shape.as_ref();
        #[cfg(feature = "tract_onnx")]
        let shape_dims = arr.shape();
        // println!("{:?}", shape_dims);

        let mut text_index: Vec<usize> = Vec::new();

        #[cfg(feature = "ort")]
        for i in 0..(shape_dims[1] as usize) {
            let mut max_index = 0;
            let mut max_value = -f32::INFINITY;
            for j in 0..(shape_dims[2] as usize) {
                // 数据是线性存储的，索引计算: [0, i, j] = i * shape[2] + j
                let idx = i * (shape_dims[2] as usize) + j;
                let value = data[idx];
                if value > max_value {
                    max_value = value;
                    max_index = j;
                }
            }
            text_index.push(max_index);
        }
        
        #[cfg(feature = "ort")]
        {
            // log::info!("[OCR调试] 输出形状: {:?}", shape_dims);
            // log::info!("[OCR调试] 预测索引序列: {:?}", text_index);
            // log::info!("[OCR调试] 预测索引序列前10个: {:?}", &text_index[..text_index.len().min(10)]);
        }
        #[cfg(feature = "tract_onnx")]
        {
            // log::info!("[OCR调试] 预测索引序列前10个: {:?}", &text_index[..text_index.len().min(10)]);
        }
        #[cfg(feature = "tract_onnx")]
        for i in 0..shape_dims[1] {
            let mut max_index = 0;
            let mut max_value = -f32::INFINITY;
            for j in 0..shape_dims[2] {
                let value = arr[[0, i, j]];
                if value > max_value {
                    max_value = value;
                    max_index = j;
                }
            }
            text_index.push(max_index);
        }

        // CTC 解码：与 Python 代码逻辑一致
        let mut s = String::new();
        let mut last_index: i32 = -1;
        let mut out_of_bounds_count = 0;
        
        for (_pos, &index) in text_index.iter().enumerate() {
            // 如果当前索引不为0且与上一个索引不同，则添加到结果
            if index != 0 && (index as i32) != last_index {
                // index 是从 1 开始的，所以访问 index_to_word[index - 1]
                // 有效范围: index ∈ [1, index_to_word.len()]
                if index == 0 || (index as usize) > self.index_to_word.len() {
                    out_of_bounds_count += 1;
                    if out_of_bounds_count == 1 {
                        // 只在第一次越界时记录详细信息
                        log_debug!("PaddleOCR 索引越界: index={}, 有效范围=[1, {}], 位置={}, 已跳过该字符",
                                   "PaddleOCR index out of bounds: index={}, valid range=[1, {}], position={}, character skipped",
                                   index, self.index_to_word.len(), _pos);
                    }
                } else {
                    let ch = &self.index_to_word[index - 1];
                    // log::info!("[OCR调试] 位置 {}: 索引 {} -> '{}'", _pos, index, ch);
                    s.push_str(ch);
                }
            }
            // 总是更新 last_index（包括0）
            last_index = index as i32;
        }
        
        if out_of_bounds_count > 0 {
            log_debug!("PaddleOCR 本次识别共有 {} 个字符索引越界被跳过，最终识别结果: '{}'", "PaddleOCR {} characters skipped due to index out of bounds, final result: '{}'", out_of_bounds_count, s);
        }
        
        // log::info!("[OCR调试] 最终识别结果: {}", s);

        // println!("{:?}", text_index);

        // let s = format!("{:?}", shape);

        Ok(s)
    }

    fn get_average_inference_time(&self) -> Option<Duration> {
        None
    }
}

#[macro_export]
macro_rules! ppocr_model {
    ($onnx:literal, $index_to_word:literal) => {
        {
            let model_bytes = include_bytes!($onnx);
            let index_to_word_str = include_str!($index_to_word);

            let mut index_to_word_vec: Vec<String> = Vec::new();
            for line in index_to_word_str.lines() {
                index_to_word_vec.push(String::from(line));
            }
            index_to_word_vec.push(String::from(" "));

            $crate::ocr::PPOCRModel::new(
                model_bytes, index_to_word_vec,
            )
        }
    };
}

pub struct PPOCRChV4RecInfer {
    model: PPOCRModel,
}

impl PPOCRChV4RecInfer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            model: ppocr_model!("./ch_PP-OCRv4_rec_infer.onnx", "./ppocr_keys_v1.txt")?
        })
    }
}

impl ImageToText<RgbImage> for PPOCRChV4RecInfer {
    fn image_to_text(&self, image: &RgbImage, is_preprocessed: bool) -> Result<String> {
        self.model.image_to_text(image, is_preprocessed)
    }

    fn get_average_inference_time(&self) -> Option<Duration> {
        self.model.get_average_inference_time()
    }
}

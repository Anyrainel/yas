use std::sync::Mutex;
use std::time::Duration;
use std::time::SystemTime;
use image::{EncodableLayout, GrayImage, ImageBuffer, Luma, RgbImage};
// use tract_onnx::prelude::*;
use crate::ocr::traits::ImageToText;
use super::preprocess;
use anyhow::Result;
use crate::common::image_ext::*;
#[cfg(feature = "ort")]
use ort::{session::{Session, builder::GraphOptimizationLevel}, value::Value};
#[cfg(feature = "tract_onnx")]
use tract_onnx::prelude::*;

#[cfg(feature = "tract_onnx")]
type ModelType = RunnableModel<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct YasOCRModel {
    #[cfg(feature = "ort")]
    model: Mutex<Session>,
    #[cfg(feature = "tract_onnx")]
    model: ModelType,
    index_to_word: Vec<String>,
}

impl YasOCRModel {
    pub fn get_average_inference_time(&self) -> Option<Duration> {
        None
    }

    pub fn new(model_bytes: &[u8], content: &str) -> Result<YasOCRModel> {
        #[cfg(feature = "ort")]
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .commit_from_memory(model_bytes)?;
        #[cfg(feature = "tract_onnx")]
        let model = tract_onnx::onnx()
            .model_for_read(&mut model_bytes.as_bytes())?
            .with_input_fact(0, f32::fact([1, 1, 32, 384]).into())?
            .into_optimized()?
            .into_runnable()?;

        let json = serde_json::from_str::<serde_json::Value>(content)?;

        let mut index_to_word = json
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.parse::<usize>().unwrap(), v.as_str().unwrap().to_string()))
            .collect::<Vec<(usize, String)>>();

        index_to_word.sort_by(|(k1, _), (k2, _)| k1.cmp(k2));

        let index_to_word = index_to_word.into_iter().map(|(_, v)| v).collect();

        Ok(YasOCRModel {
            #[cfg(feature = "ort")]
            model: Mutex::new(session),
            #[cfg(feature = "tract_onnx")]
            model,
            index_to_word,
        })
    }

    pub fn inference_string(&self, img: &ImageBuffer<Luma<f32>, Vec<f32>>) -> Result<String> {
        #[cfg(feature = "ort")]
        let tensor_array = ndarray::Array4::from_shape_fn((1, 1, 32, 384), |(_, _, y, x)| {
            img.get_pixel(x as u32, y as u32)[0]
        });
        #[cfg(feature = "tract_onnx")]
        let tensor: Tensor =
            tract_ndarray::Array4::from_shape_fn((1, 1, 32, 384), |(_, _, y, x)| {
                img.get_pixel(x as u32, y as u32)[0]
            }).into();

        #[cfg(feature = "ort")]
        let tensor_value = Value::from_array(tensor_array)?;
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

        let mut ans = String::new();
        let mut last_word = String::new();
        
        #[cfg(feature = "ort")]
        for i in 0..shape_dims[0] as usize {
            let mut max_index = 0;
            let mut max_value = -1.0_f32;
            for j in 0..self.index_to_word.len() {
                // 数据存储顺序是 [batch, height, width]，即 [i, 0, j]
                let idx = i * (shape_dims[2] as usize) + j;
                let value = data[idx];
                if value > max_value {
                    max_value = value;
                    max_index = j;
                }
            }
            let word = &self.index_to_word[max_index];
            if *word != last_word && word != "-" {
                ans = ans + word;
            }

            last_word.clone_from(word);
        }
        
        #[cfg(feature = "tract_onnx")]
        for i in 0..shape_dims[0] {
            let mut max_index = 0;
            let mut max_value = -1.0_f32;
            for j in 0..self.index_to_word.len() {
                let value = arr[[i, 0, j]];
                if value > max_value {
                    max_value = value;
                    max_index = j;
                }
            }
            let word = &self.index_to_word[max_index];
            if *word != last_word && word != "-" {
                ans = ans + word;
            }

            last_word.clone_from(word);
        }

        Ok(ans)
    }
}

impl ImageToText<RgbImage> for YasOCRModel {
    fn image_to_text(&self, image: &RgbImage, is_preprocessed: bool) -> Result<String> {
        assert!(!is_preprocessed);

        let gray_image_float = preprocess::to_gray(image);
        let (result, non_mono) = preprocess::pre_process(gray_image_float);

        if !non_mono {
            return Ok(String::new());
        }

        let string_result = self.inference_string(&result)?;

        Ok(string_result)
    }

    fn get_average_inference_time(&self) -> Option<Duration> {
        None
    }
}

impl ImageToText<ImageBuffer<Luma<f32>, Vec<f32>>> for YasOCRModel {
    fn image_to_text(&self, image: &ImageBuffer<Luma<f32>, Vec<f32>>, is_preprocessed: bool) -> Result<String> {
        if is_preprocessed {
            let string_result = self.inference_string(image)?;
            Ok(string_result)
        } else {
            let im = image.clone();
            let (preprocess_result, non_mono) = preprocess::pre_process(im);

            if !non_mono {
                return Ok(String::new());
            }

            let string_result = self.inference_string(&preprocess_result)?;
            Ok(string_result)
        }
    }

    fn get_average_inference_time(&self) -> Option<Duration> {
        None
    }
}

impl ImageToText<GrayImage> for YasOCRModel {
    fn image_to_text(&self, im: &GrayImage, is_preprocessed: bool) -> Result<String> {
        let gray_f32_image: ImageBuffer<Luma<f32>, Vec<f32>> = im.to_f32_gray_image();
        self.image_to_text(&gray_f32_image, is_preprocessed)
    }

    fn get_average_inference_time(&self) -> Option<Duration> {
        None
    }
}

#[macro_export]
macro_rules! yas_ocr_model {
    ($model_name:literal, $index_to_word:literal) => {
        {
            let model_bytes = include_bytes!($model_name);
            let index_to_word = include_str!($index_to_word);

            $crate::ocr::YasOCRModel::new(
                model_bytes, index_to_word,
            )
        }
    };
}

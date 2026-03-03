use std::collections::{HashSet, BTreeMap};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use image::Rgb;
use image::{GenericImageView, RgbImage};
use log::{error, info, warn};
use indicatif::{ProgressBar, ProgressStyle};

use yas::ocr::ImageToText;
use yas::ocr::yas_ocr_model;
use yas::ocr::PPOCRModel;
use yas::positioning::{Pos, Rect};
use yas::utils::color_distance;

use crate::scanner::artifact_scanner::artifact_scanner_window_info::ArtifactScannerWindowInfo;
use crate::scanner::artifact_scanner::GenshinArtifactScannerConfig;
use crate::scanner::artifact_scanner::message_items::SendItem;
use crate::scanner::artifact_scanner::scan_result::GenshinArtifactScanResult;

/// Save image for debugging purposes
fn save_debug_image(image: &RgbImage, artifact_index: usize, region_tag: &str) -> Result<()> {
    // Create debug directory if it doesn't exist
    let debug_dir = Path::new("./debug_images");
    if !debug_dir.exists() {
        std::fs::create_dir_all(debug_dir)?;
    }

    let full_filename = format!("artifact_{}_{}.png", artifact_index, region_tag);
    let image_path = debug_dir.join(full_filename);

    image.save(&image_path)?;
    info!("Debug image saved: {}", image_path.display());

    Ok(())
}

/// run in a separate thread, accept captured image and get an artifact
pub struct ArtifactScannerWorker {
    model: Box<dyn ImageToText<RgbImage> + Send + Sync>,
    paddle_model: Option<Box<dyn ImageToText<RgbImage> + Send + Sync>>,
    paddlev3_model: Option<Box<dyn ImageToText<RgbImage> + Send + Sync>>,  // PaddleOCR v3 模型
    yas_model: Option<Box<dyn ImageToText<RgbImage> + Send + Sync>>,
    window_info: ArtifactScannerWindowInfo,
    config: GenshinArtifactScannerConfig,
}

impl ArtifactScannerWorker {
    fn get_model_for_backend(backend: &str) -> Result<Box<dyn ImageToText<RgbImage> + Send + Sync>> {
        match backend.to_lowercase().as_str() {
            "paddle" | "ppocrv5" => {
                let model_bytes = include_bytes!("./models/PP-OCRv5_mobile_rec.onnx");
                let dict_str = include_str!("./models/ppocrv5_dict.txt");
                let mut dict_vec: Vec<String> = dict_str.lines().map(|l| l.trim().to_string()).collect();
                // PaddleOCR 字典需要在末尾添加空格字符
                dict_vec.push(String::from(" "));
                let model = PPOCRModel::new(model_bytes, dict_vec)?;
                Ok(Box::new(model))
            },
            "paddlev3" | "ppocrv3" => {
                let model_bytes = include_bytes!("./models/ch_PP-OCRv3_rec_infer.onnx");
                let dict_str = include_str!("./models/ppocr_keys_v1.txt");
                let mut dict_vec: Vec<String> = dict_str.lines().map(|l| l.trim().to_string()).collect();
                // PaddleOCR 字典需要在末尾添加空格字符
                dict_vec.push(String::from(" "));
                let model = PPOCRModel::new(model_bytes, dict_vec)?;
                Ok(Box::new(model))
            },
            _ => {
                let model: Box<dyn ImageToText<RgbImage> + Send + Sync> = Box::new(
                    yas_ocr_model!("./models/model_training.onnx", "./models/index_2_word.json")?
                );
                Ok(model)
            }
        }
    }

    fn parse_level(s: &str) -> Result<i32> {
        // 自动容错：将 o/O 替换为 0，只保留数字和正负号
        let replaced = s.replace(['o', 'O'], "0");
        let cleaned: String = replaced.chars().filter(|c| c.is_ascii_digit() || *c == '-' || *c == '+').collect();
        let cleaned = cleaned.trim_start_matches('+');
        if cleaned.is_empty() {
            return Ok(0);
        }
        match cleaned.parse::<i32>() {
            Ok(level) => Ok(level),
            Err(e) => {
                log::warn!("parse_level 解析失败: 原始内容='{}', o/O->0后='{}', 错误: {}，自动返回0", s, cleaned, e);
                Ok(0)
            }
        }
    }

    /// 处理词条文本，去除“（”及其后内容，如果包含“（”则视为包含预览词条，标记为 true
    fn process_stat_text(s: &str) -> (String, bool) {
        match s.find('（') {
            Some(idx) => (s[..idx].trim_end().to_string(), true),
            None => (s.trim().to_string(), false),
        }
    }

    pub fn new(
        window_info: ArtifactScannerWindowInfo,
        config: GenshinArtifactScannerConfig,
    ) -> Result<Self> {
        let model = Self::get_model_for_backend(&config.ocr_backend)?;
        
        // paddle_model 用于 title 识别,始终加载以确保 title 使用 PaddleOCR v5
        let paddle_model = Some(Self::get_model_for_backend("paddle")?);
        
        let paddlev3_model = if config.ocr_backend.to_lowercase() == "paddlev3" 
            || config.ocr_backend.to_lowercase() == "ppocrv3"
            || config.substat4_ocr_backend.to_lowercase() == "paddlev3" 
            || config.substat4_ocr_backend.to_lowercase() == "ppocrv3" {
            Some(Self::get_model_for_backend("paddlev3")?)
        } else {
            None
        };
        
        let yas_model = if config.ocr_backend.to_lowercase() == "yas" || config.substat4_ocr_backend.to_lowercase() == "yas" {
            Some(Self::get_model_for_backend("yas")?)
        } else {
            None
        };
        
        Ok(ArtifactScannerWorker {
            model,
            paddle_model,
            paddlev3_model,
            yas_model,
            window_info,
            config,
        })
    }

    /// the captured_img is a panel of the artifact, the rect is a region of the panel
    fn model_inference(&self, rect: Rect<f64>, captured_img: &RgbImage, artifact_index: usize, region_tag: &str) -> Result<String> {
        let relative_rect = rect.translate(Pos {
            x: -self.window_info.panel_rect.left,
            y: -self.window_info.panel_rect.top,
        });

        let raw_img = captured_img.view(
            relative_rect.left as u32, relative_rect.top as u32, relative_rect.width as u32, relative_rect.height as u32,
        ).to_image();

        if self.config.save_images {
            if let Err(e) = save_debug_image(&raw_img, artifact_index, region_tag) {
                warn!("Failed to save region debug image: {}", e);
            }
        }

        let inference_result = self.model.image_to_text(&raw_img, false);
        
        // 添加错误上下文信息
        match inference_result {
            Ok(text) => Ok(text),
            Err(e) => {
                error!("OCR识别失败: artifact_index={}, region_tag='{}', 错误: {:?}", artifact_index, region_tag, e);
                Err(e)
            }
        }
    }

    /// Detect if the artifact has "祝圣" text which shifts all stats down by one line
    fn detect_consecration_shift(&self, image: &RgbImage, artifact_index: usize) -> f64 {
        // 检测区域：在 main_stat_value 和 sub_stat_1 之间（祝圣文本应该出现在这里）
        // 使用一个小的检测区域来识别是否有"祝圣"关键字
        // 向下偏移一个词条高度来定位祝圣文本的实际位置
        let substat_line_height = self.window_info.sub_stat_2.top - self.window_info.sub_stat_1.top;
        let detect_rect = Rect {
            left: self.window_info.main_stat_value_rect.left,
            top: self.window_info.main_stat_value_rect.top + self.window_info.main_stat_value_rect.height + substat_line_height + self.window_info.item_equip_rect.height,
            width: self.window_info.main_stat_value_rect.width,
            height: substat_line_height, // 检测一个小区域高度（祝圣文本大约一行的高度）
        };
        
        let relative_rect = detect_rect.translate(Pos {
            x: -self.window_info.panel_rect.left,
            y: -self.window_info.panel_rect.top,
        });
        
        // 安全检查：确保区域在图像范围内
        if relative_rect.left < 0.0 || relative_rect.top < 0.0 
            || (relative_rect.left + relative_rect.width) as u32 > image.width()
            || (relative_rect.top + relative_rect.height) as u32 > image.height() {
            return 0.0;
        }
        
        let raw_img = image.view(
            relative_rect.left as u32,
            relative_rect.top as u32,
            relative_rect.width as u32,
            relative_rect.height as u32,
        ).to_image();
        
        if self.config.save_images {
            if let Err(e) = save_debug_image(&raw_img, artifact_index, "consecration_detect") {
                warn!("Failed to save consecration detection image: {}", e);
            }
        }
        
        // 使用 paddle 模型识别（更准确）
        if let Some(model) = &self.paddle_model {
            match model.image_to_text(&raw_img, false) {
                Ok(text) => {
                    let normalized = text.chars().filter(|c| !c.is_whitespace()).collect::<String>();
                    // 检测祝圣关键字（各种可能的OCR结果）
                    if normalized.contains("祝圣") || normalized.contains("之霜") || normalized.contains("之油") 
                        || normalized.contains("之露") || normalized.contains("之葩") || normalized.contains("定义") {
                        if self.config.verbose {
                            info!("[artifact_index={}] 检测到祝圣文本: '{}', 应用位置偏移", artifact_index, text);
                        }
                        // 返回偏移量：大约一个副词条的高度
                        return self.window_info.sub_stat_2.top - self.window_info.sub_stat_1.top;
                    }
                },
                Err(e) => {
                    warn!("OCR识别失败: artifact_index={}, region_tag='consecration_detect', 错误: {:?}", artifact_index, e);
                }
            }
        }
        
        0.0 // 没有检测到祝圣文本，不偏移
    }

    /// Parse the captured result (of type SendItem) to a scanned artifact
    fn scan_item_image(&self, item: SendItem, lock: bool, artifact_index: usize) -> Result<GenshinArtifactScanResult> {
        let image = &item.panel_image;

        let str_title = {
            // 使用 PaddleOCR v5 模型识别 title (圣遗物名称)
            let model: &Box<dyn ImageToText<RgbImage> + Send + Sync> = self.paddle_model.as_ref().expect("paddle_model should be initialized for title recognition");
            let relative_rect = self.window_info.title_rect.translate(Pos {
                x: -self.window_info.panel_rect.left,
                y: -self.window_info.panel_rect.top,
            });
            let raw_img = image.view(
                relative_rect.left as u32, relative_rect.top as u32, relative_rect.width as u32, relative_rect.height as u32,
            ).to_image();
            
            if self.config.save_images {
                if let Err(e) = save_debug_image(&raw_img, artifact_index, "title") {
                    warn!("Failed to save region debug image: {}", e);
                }
            }
            
            model.image_to_text(&raw_img, false)?
        };

        // 如果是祝圣精华等非圣遗物，提前返回，避免后续解析错误
        if str_title.contains("祝圣精华") || str_title.contains("祝圣油膏") {
            return anyhow::Ok(GenshinArtifactScanResult {
                name: str_title,
                main_stat_name: String::new(),
                main_stat_value: String::new(),
                sub_stat: [String::new(), String::new(), String::new(), String::new()],
                level: 0,
                equip: String::new(),
                star: item.star as i32,
                lock,
                index: artifact_index,
            });
        }

        let str_main_stat_name = self.model_inference(self.window_info.main_stat_name_rect, image, artifact_index, "main_stat_name")?;
        let str_main_stat_value = self.model_inference(self.window_info.main_stat_value_rect, image, artifact_index, "main_stat_value")?;
        
        // 检测是否有祝圣文本，如果有则计算需要向下偏移的距离
        // 只有 5 星圣遗物才可能有祝圣属性偏移
        let shift_offset = if item.star == 5 {
            self.detect_consecration_shift(image, artifact_index)
        } else {
            0.0
        };
        
        // 应用偏移后的副词条区域
        let sub_stat_1_rect = self.window_info.sub_stat_1.translate(Pos { x: 0.0, y: shift_offset });
        let sub_stat_2_rect = self.window_info.sub_stat_2.translate(Pos { x: 0.0, y: shift_offset });
        let sub_stat_3_rect = self.window_info.sub_stat_3.translate(Pos { x: 0.0, y: shift_offset });
        let sub_stat_4_rect = self.window_info.sub_stat_4.translate(Pos { x: 0.0, y: shift_offset });
        let level_rect = self.window_info.level_rect.translate(Pos { x: 0.0, y: shift_offset });
        
        let mut has_unactivated = false;

        let (str_sub_stat0, u0) = Self::process_stat_text(&self.model_inference(sub_stat_1_rect, image, artifact_index, "substat1")?);
        has_unactivated |= u0;

        let (str_sub_stat1, u1) = Self::process_stat_text(&self.model_inference(sub_stat_2_rect, image, artifact_index, "substat2")?);
        has_unactivated |= u1;

        let (str_sub_stat2, u2) = Self::process_stat_text(&self.model_inference(sub_stat_3_rect, image, artifact_index, "substat3")?);
        has_unactivated |= u2;

        let (str_sub_stat3, u3) = if !self.config.substat4_ocr_backend.is_empty() {
            let backend = self.config.substat4_ocr_backend.to_lowercase();
            let model: &Box<dyn ImageToText<RgbImage> + Send + Sync> = match backend.as_str() {
                "paddle" | "ppocrv5" => self.paddle_model.as_ref().expect("paddle_model should be initialized"),
                "paddlev3" | "ppocrv3" => self.paddlev3_model.as_ref().expect("paddlev3_model should be initialized"),
                "yas" => self.yas_model.as_ref().expect("yas_model should be initialized"),
                _ => &self.model,
            };
            let relative_rect = sub_stat_4_rect.translate(Pos {
                x: -self.window_info.panel_rect.left,
                y: -self.window_info.panel_rect.top,
            });
            let raw_img = image.view(
                relative_rect.left as u32, relative_rect.top as u32, relative_rect.width as u32, relative_rect.height as u32,
            ).to_image();
            
            if self.config.save_images {
                if let Err(e) = save_debug_image(&raw_img, artifact_index, "substat4") {
                    warn!("Failed to save region debug image: {}", e);
                }
            }
            
            let raw = model.image_to_text(&raw_img, false)?;
            Self::process_stat_text(&raw)
        } else {
            let raw = self.model_inference(sub_stat_4_rect, image, artifact_index, "substat4")?;
            Self::process_stat_text(&raw)
        };
        has_unactivated |= u3;

        let str_level = self.model_inference(level_rect, image, artifact_index, "level")?;
        let str_equip = {
            // 使用 PaddleOCR v5 模型识别装备者，减少繁/简体和错字识别差异
            let relative_rect = self.window_info.item_equip_rect.translate(Pos {
                x: -self.window_info.panel_rect.left,
                y: -self.window_info.panel_rect.top,
            });
            let raw_img = image.view(
                relative_rect.left as u32,
                relative_rect.top as u32,
                relative_rect.width as u32,
                relative_rect.height as u32,
            ).to_image();

            if self.config.save_images {
                if let Err(e) = save_debug_image(&raw_img, artifact_index, "equip") {
                    warn!("Failed to save region debug image: {}", e);
                }
            }

            let model: &Box<dyn ImageToText<RgbImage> + Send + Sync> = self.paddle_model.as_ref().expect("paddle_model should be initialized for equip recognition");
            // Resize equip region to fixed size (320x48) to match Python preprocessing, then mark as preprocessed
            let fixed = image::imageops::resize(&raw_img, 320, 48, image::imageops::FilterType::Triangle);
            model.image_to_text(&fixed, true)?
        };

        anyhow::Ok(GenshinArtifactScanResult {
            name: str_title,
            main_stat_name: str_main_stat_name,
            main_stat_value: str_main_stat_value,
            sub_stat: [
                str_sub_stat0,
                str_sub_stat1,
                str_sub_stat2,
                str_sub_stat3,
            ],
            level: {
                let mut lv = Self::parse_level(&str_level)?;
                if has_unactivated {
                    lv = 4;
                }
                lv
            },
            equip: str_equip,
            star: item.star as i32,
            lock,
            index: artifact_index,
        })
    }

    /// Get all lock state from a list image
    fn get_page_locks(&self, list_image: &RgbImage) -> Vec<bool> {
        let mut result = Vec::new();

        let row = self.window_info.row;
        let col = self.window_info.col;
        let gap = self.window_info.item_gap_size;
        let size = self.window_info.item_size;
        let lock_pos = self.window_info.lock_pos;

        if self.config.save_images {
            if let Err(e) = save_debug_image(list_image, 0, "list_page") {
                warn!("Failed to save list page debug image: {}", e);
            }
        }

        for r in 0..row {
            if ((gap.height + size.height) * (r as f64)) as u32 > list_image.height() {
                break;
            }
            for c in 0..col {
                let pos_x = (gap.width + size.width) * (c as f64) + lock_pos.x;
                let pos_y = (gap.height + size.height) * (r as f64) + lock_pos.y;

                let mut locked = false;
                'sq: for dx in -1..1 {
                    for dy in -10..10 {
                        if pos_y as i32 + dy < 0 || (pos_y as i32 + dy) as u32 >= list_image.height() {
                            continue;
                        }

                        let color = list_image
                            .get_pixel((pos_x as i32 + dx) as u32, (pos_y as i32 + dy) as u32);

                        if color_distance(color, &Rgb([255, 138, 117])) < 30 {
                            locked = true;
                            break 'sq;
                        }
                    }
                }
                result.push(locked);
            }
        }
        result
    }

    pub fn run(self, rx: Receiver<Option<SendItem>>, total_count: usize) -> JoinHandle<Vec<GenshinArtifactScanResult>> {
        let worker = Arc::new(self);
        std::thread::spawn(move || {
            let mut results = Vec::new();
            let mut hash: HashSet<GenshinArtifactScanResult> = HashSet::new();
            // if too many artifacts are same in consecutive, then an error has occurred
            let mut consecutive_dup_count = 0;

            let is_verbose = worker.config.verbose;
            let min_level = worker.config.min_level;
            let info = worker.window_info.clone();

            let pb = ProgressBar::new(total_count as u64);
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos:>7}/{len:7} ({eta})")
                .unwrap()
                .progress_chars("#>-"));

            let (tx_ocr, rx_ocr) = channel();
            let worker_clone = worker.clone();

            // 启动一个线程来接收图像并分发 OCR 任务
            std::thread::spawn(move || {
                let mut locks = Vec::new();
                let mut items_count = 0;
                for item in rx {
                    let item = match item {
                        Some(v) => v,
                        None => break,
                    };
                    
                    // 如果有列表图像，更新锁定状态缓存
                    if let Some(v) = item.list_image.as_ref() {
                        locks.extend(worker_clone.get_page_locks(v));
                    }

                    let index = items_count;
                    items_count += 1;
                    let lock = if index < locks.len() { locks[index] } else { false };
                    
                    let worker_inner = worker_clone.clone();
                    let tx_ocr_inner = tx_ocr.clone();
                    
                    rayon::spawn(move || {
                        let res = worker_inner.scan_item_image(item, lock, index + 1);
                        let _ = tx_ocr_inner.send((index, res));
                    });
                }
            });

            let mut results_map = BTreeMap::new();
            let mut next_index = 0;

            // 按顺序处理 OCR 结果
            loop {
                // 如果结果映射中没有下一个索引的结果，则从通道中接收
                while !results_map.contains_key(&next_index) {
                    match rx_ocr.recv() {
                        Ok((i, res)) => {
                            results_map.insert(i, res);
                        },
                        Err(_) => break,
                    }
                }

                let result = match results_map.remove(&next_index) {
                    Some(res) => {
                        next_index += 1;
                        match res {
                            Ok(v) => v,
                            Err(e) => {
                                error!("识别错误: {}", e);
                                pb.inc(1);
                                continue;
                            }
                        }
                    },
                    None => break, // 通道已关闭且没有更多结果
                };

                // 过滤非圣遗物物品（如祝圣精华、祝圣油膏）
                if result.name.contains("祝圣精华") || result.name.contains("祝圣油膏") {
                    if is_verbose {
                        info!("跳过非圣遗物物品: {}", result.name);
                    }
                    pb.inc(1);
                    continue;
                }

                if is_verbose {
                    info!("{:?}", result);
                }
                // 尝试转换为 GenshinArtifact，若失败输出详细原因
                if let Err(e) = crate::artifact::GenshinArtifact::try_from(&result) {
                    error!(
                        "artifact_index={} GenshinArtifact::try_from 失败，name='{}', main_stat_name='{}', main_stat_value='{}', sub_stat4='{}'，原因: {:?}",
                        next_index,
                        result.name,
                        result.main_stat_name,
                        result.main_stat_value,
                        result.sub_stat[3],
                        e
                    );
                }

                if result.level < min_level {
                    info!(
                        "扫描终止：找到满足最低等级要求 {} 的物品({})，已扫描 {} 个。",
                        min_level, result.level, next_index
                    );
                    pb.finish_with_message("终止");
                    break;
                }

                if hash.contains(&result) {
                    consecutive_dup_count += 1;
                    warn!("识别到重复物品: {:#?}", result);
                } else {
                    consecutive_dup_count = 0;
                    hash.insert(result.clone());
                    results.push(result);
                }

                pb.inc(1);

                if consecutive_dup_count >= info.col && !worker.config.ignore_dup {
                    error!(
                        "扫描终止：识别到连续 {} 个重复物品，可能为翻页错误，或者为非背包顶部开始扫描，已扫描 {} 个。",
                        consecutive_dup_count, next_index
                    );
                    pb.finish_with_message("重复终止");
                    break;
                }

                if next_index >= total_count {
                    break;
                }
            }

            pb.finish_with_message("完成");
            info!("识别结束，非重复物品数量: {}", hash.len());

            results
        })
    }
}

//! muse-engine：MuseAI 宿主无关核心引擎。
//!
//! 约束（规格 §8.2 / §16）：本 crate 不依赖 tauri / axum 任何类型；
//! 文件、时钟、事件、模型调用一律通过 [`host`] 与 [`model`] 的 trait 注入。
//! 桌面壳（src-tauri）与平台后端（server）共享同一套实现。

pub mod character;
pub mod error;
pub mod host;
pub mod knowledge;
pub mod model;
pub mod narrative;
pub mod store;
pub mod world;

pub use error::EngineError;

/// 引擎版本号，随不兼容的管线/状态变更递增；持久化对象与事件携带它用于版本钉住。
pub const ENGINE_VERSION: &str = "0.1.0";

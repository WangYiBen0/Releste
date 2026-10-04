//! 管线错误。

/// 资源转换错误。
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("PNG decode error at {path}: {source}")]
    Png {
        path: String,
        source: image::ImageError,
    },
    #[error("{format} parse error at {path}: {message}")]
    Parse {
        format: &'static str,
        path: String,
        message: String,
    },
    #[error("destination path escapes output directory: {0}")]
    PathEscape(String),
    #[error("serialize error: {0}")]
    Serialize(String),
}

impl PipelineError {
    /// 构造 I/O 错误。
    pub fn io(path: impl std::fmt::Display, source: std::io::Error) -> Self {
        PipelineError::Io {
            path: path.to_string(),
            source,
        }
    }

    /// 构造解析错误。
    pub fn parse(
        format: &'static str,
        path: impl std::fmt::Display,
        message: impl Into<String>,
    ) -> Self {
        PipelineError::Parse {
            format,
            path: path.to_string(),
            message: message.into(),
        }
    }
}

impl From<reles_map::MapError> for PipelineError {
    fn from(e: reles_map::MapError) -> Self {
        PipelineError::Serialize(format!("map format: {e}"))
    }
}

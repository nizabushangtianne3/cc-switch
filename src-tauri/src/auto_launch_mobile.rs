use crate::error::AppError;

pub fn enable_auto_launch() -> Result<(), AppError> {
    Err(AppError::Message("移动端暂不支持开机自启".to_string()))
}

pub fn disable_auto_launch() -> Result<(), AppError> {
    Err(AppError::Message("移动端暂不支持开机自启".to_string()))
}

pub fn is_auto_launch_enabled() -> Result<bool, AppError> {
    Ok(false)
}

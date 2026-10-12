use crate::GatewayError;

pub(crate) fn identifier(value: &str) -> Result<(), GatewayError> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > 128
        || value.chars().any(char::is_control)
    {
        return Err(GatewayError::InvalidInput("invalid identifier"));
    }
    Ok(())
}

pub(crate) fn text(value: &str, maximum: usize) -> Result<(), GatewayError> {
    if value.trim().is_empty() || value.chars().count() > maximum || value.contains('\0') {
        return Err(GatewayError::InvalidInput("invalid text"));
    }
    Ok(())
}

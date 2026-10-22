/// Nome comercial de um iPad a partir do `ProductType` (só os modelos com ProMotion e alguns comuns).
pub fn model_name(product_type: &str) -> Option<&'static str> {
    Some(match product_type {
        "iPad7,1" | "iPad7,2" => "iPad Pro 12,9\" (2.ª geração)",
        "iPad7,3" | "iPad7,4" => "iPad Pro 10,5\"",
        "iPad8,1" | "iPad8,2" | "iPad8,3" | "iPad8,4" => "iPad Pro 11\" (2018)",
        "iPad8,5" | "iPad8,6" | "iPad8,7" | "iPad8,8" => "iPad Pro 12,9\" (3.ª geração)",
        "iPad8,9" | "iPad8,10" => "iPad Pro 11\" (2.ª geração)",
        "iPad8,11" | "iPad8,12" => "iPad Pro 12,9\" (4.ª geração)",
        "iPad13,4" | "iPad13,5" | "iPad13,6" | "iPad13,7" => "iPad Pro 11\" (3.ª geração, M1)",
        "iPad13,8" | "iPad13,9" | "iPad13,10" | "iPad13,11" => "iPad Pro 12,9\" (5.ª geração, M1)",
        "iPad14,3" | "iPad14,4" => "iPad Pro 11\" (4.ª geração, M2)",
        "iPad14,5" | "iPad14,6" => "iPad Pro 12,9\" (6.ª geração, M2)",
        "iPad16,3" | "iPad16,4" => "iPad Pro 11\" (M4)",
        "iPad16,5" | "iPad16,6" => "iPad Pro 13\" (M4)",
        _ => return None,
    })
}

/// Nome para mostrar: comercial se conhecido, senão o `ProductType`, senão o genérico.
pub(crate) fn display_name(product_type: Option<&str>, fallback: &str) -> String {
    match product_type {
        Some(p) => model_name(p).map_or_else(|| p.to_string(), str::to_string),
        None => fallback.to_string(),
    }
}

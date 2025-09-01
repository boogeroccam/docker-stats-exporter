use anyhow::{anyhow, Result};
use lazy_static::lazy_static;
use std::collections::HashMap;

lazy_static! {
	static ref UNIT_MAP: HashMap<&'static str, f64> = {
		let mut map = HashMap::new();
		// Base unit
		map.insert("B", 1f64);

		// Decimal units (SI)
		map.insert("kB", 1000f64);
		map.insert("MB", 1000f64 * 1000f64);
		map.insert("GB", 1000f64 * 1000f64 * 1000f64);
		map.insert("TB", 1000f64 * 1000f64 * 1000f64 * 1000f64);

		// Binary units (IEC)
		map.insert("KiB", 1024f64);
		map.insert("MiB", 1024f64 * 1024f64);
		map.insert("GiB", 1024f64 * 1024f64 * 1024f64);
		map.insert("TiB", 1024f64 * 1024f64 * 1024f64 * 1024f64);

		map
	};
}

pub fn convert_to_bytes(value: f64, unit: String) -> Result<f64> {
	let Some(conversion_rate) = UNIT_MAP.get(unit.as_str()) else {
		return Err(anyhow!("Couldn't convert unit '{}' to bytes.", unit));
	};

	let result = conversion_rate * value;
	Ok(result)
}

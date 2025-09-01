mod convert_to_bytes;
mod docker;
mod error;

use crate::convert_to_bytes::convert_to_bytes;
use crate::docker::DockerContainerStats;
use crate::error::ApiResult;
use anyhow::{anyhow, Result};
use axum::{routing::get, Router};
use clap::Parser;
use prometheus::core::{AtomicF64, GenericGauge};
use prometheus::{Encoder, Gauge, Opts, Registry, TextEncoder};
use std::collections::HashMap;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
	/// Address to bind the server to
	#[arg(short, long, default_value = "0.0.0.0:3069")]
	bind_address: String,

	/// Labels to add to all metrics in format "key1=value1,key2=value2"
	#[arg(short, long)]
	labels: Option<String>,

	/// Log level (error, warn, info, debug, trace)
	#[arg(long, default_value = "info")]
	log_level: String,

	/// Number of worker threads for the runtime
	#[arg(short, long, default_value = "4")]
	threads: usize,
}

fn parse_labels(labels_str: Option<String>) -> Result<HashMap<String, String>> {
	let mut labels = HashMap::new();

	if let Some(labels_str) = labels_str {
		for pair in labels_str.split(',') {
			let mut parts = pair.split('=');
			let key = parts
				.next()
				.ok_or_else(|| anyhow!("Invalid label format: {}", pair))?;
			let value = parts
				.next()
				.ok_or_else(|| anyhow!("Invalid label format: {}", pair))?;

			if parts.next().is_some() {
				return Err(anyhow!(
					"Invalid label format (too many '=' characters): {}",
					pair
				));
			}

			labels.insert(key.trim().to_string(), value.trim().to_string());
		}
	}

	Ok(labels)
}

fn percent_gauge(
	name: String,
	mut percent_string: String,
	help: String,
	container_name: &str,
	labels: &HashMap<String, String>,
) -> Result<GenericGauge<AtomicF64>> {
	percent_string.pop();
	let value: f64 = percent_string.parse()?;
	get_gauge(name, help, value, container_name, labels)
}

fn get_gauge(
	name: String,
	help: String,
	value: f64,
	container_name: &str,
	labels: &HashMap<String, String>,
) -> Result<GenericGauge<AtomicF64>> {
	let mut opts = Opts::new(name.replace("-", "_"), help);

	// Add container name as a label
	opts = opts.const_label("container", container_name);

	// Add user-defined labels
	for (key, val) in labels {
		opts = opts.const_label(key, val);
	}

	let gauge = Gauge::with_opts(opts)?;
	gauge.set(value);
	Ok(gauge)
}

fn parse_io_str(str: String) -> Result<f64> {
	let backwards_unit = str
		.chars()
		.rev()
		.take_while(|c| c.is_alphabetic())
		.collect::<String>();
	let unit = backwards_unit.chars().rev().collect::<String>();
	let index = str.len() - unit.len();
	let value = &str[0..index];
	let float_value = value.parse::<f64>()?;
	let result = convert_to_bytes(float_value, unit)?;
	Ok(result)
}

fn parse_netio_str(netio_string: &str) -> Result<(f64, f64)> {
	let mut input_output: Vec<&str> = netio_string.split(" / ").collect();
	let (Some(output), Some(input)) = (input_output.pop(), input_output.pop()) else {
		return Err(anyhow!("Bad netio string: '{}'", netio_string));
	};

	let inp = parse_io_str(input.to_string())?;
	let out = parse_io_str(output.to_string())?;

	Ok((inp, out))
}

fn parse_blockio_str(blockio_string: &str) -> Result<(f64, f64)> {
	let mut input_output: Vec<&str> = blockio_string.split(" / ").collect();
	let (Some(output), Some(input)) = (input_output.pop(), input_output.pop()) else {
		return Err(anyhow!("Bad block IO string: '{}'", blockio_string));
	};

	let inp = parse_io_str(input.to_string())?;
	let out = parse_io_str(output.to_string())?;

	Ok((inp, out))
}

fn parse_mem_usage_str(mem_usage_string: &str) -> Result<(f64, f64)> {
	let mut usage_limit: Vec<&str> = mem_usage_string.split(" / ").collect();
	let (Some(limit), Some(usage)) = (usage_limit.pop(), usage_limit.pop()) else {
		return Err(anyhow!("Bad memory usage string: '{}'", mem_usage_string));
	};

	let usage_bytes = parse_io_str(usage.to_string())?;
	let limit_bytes = parse_io_str(limit.to_string())?;

	Ok((usage_bytes, limit_bytes))
}

fn gauges_for_container(
	stat: &DockerContainerStats,
	labels: &HashMap<String, String>,
) -> Result<Vec<GenericGauge<AtomicF64>>> {
	let cpu_gauge = percent_gauge(
		"container_cpu_usage".to_string(),
		stat.cpu_perc.clone(),
		"CPU usage percentage for container".to_string(),
		&stat.container,
		labels,
	)?;
	let (mem_usage_bytes, mem_limit_bytes) = parse_mem_usage_str(stat.mem_usage.as_str())?;
	let mem_usage_gauge = get_gauge(
		"container_memory_usage_bytes".to_string(),
		"Memory usage in bytes for container".to_string(),
		mem_usage_bytes,
		&stat.container,
		labels,
	)?;
	let mem_limit_gauge = get_gauge(
		"container_memory_limit_bytes".to_string(),
		"Memory limit in bytes for container".to_string(),
		mem_limit_bytes,
		&stat.container,
		labels,
	)?;
	let (input, output) = parse_netio_str(stat.net_io.as_str())?;
	let net_input_gauge = get_gauge(
		"container_network_input_bytes".to_string(),
		"Network input bytes for container".to_string(),
		input,
		&stat.container,
		labels,
	)?;
	let net_output_gauge = get_gauge(
		"container_network_output_bytes".to_string(),
		"Network output bytes for container".to_string(),
		output,
		&stat.container,
		labels,
	)?;

	let (block_read, block_write) = parse_blockio_str(stat.block_io.as_str())?;
	let block_read_gauge = get_gauge(
		"container_block_read_bytes".to_string(),
		"Block read bytes for container".to_string(),
		block_read,
		&stat.container,
		labels,
	)?;
	let block_write_gauge = get_gauge(
		"container_block_write_bytes".to_string(),
		"Block write bytes for container".to_string(),
		block_write,
		&stat.container,
		labels,
	)?;

	Ok(vec![
		cpu_gauge,
		mem_usage_gauge,
		mem_limit_gauge,
		net_input_gauge,
		net_output_gauge,
		block_read_gauge,
		block_write_gauge,
	])
}

fn get_prometheus_format(
	stats: Vec<DockerContainerStats>,
	labels: &HashMap<String, String>,
) -> Result<String> {
	let registry = Registry::new();
	for container_stats in &stats {
		for gauge in gauges_for_container(container_stats, labels)? {
			registry.register(Box::new(gauge))?;
		}
	}

	let mut buffer = vec![];
	let encoder = TextEncoder::new();
	let metric_families = registry.gather();
	encoder.encode(&metric_families, &mut buffer)?;

	let str = String::from_utf8(buffer)?;
	Ok(str)
}

async fn docker_stats_metrics(labels: HashMap<String, String>) -> ApiResult<String> {
	let stats = docker::stats()?;
	let prometheus_stuff = get_prometheus_format(stats, &labels)?;
	Ok(prometheus_stuff)
}

fn main() -> Result<()> {
	let args = Args::parse();
	let labels = parse_labels(args.labels.clone())?;

	// Create custom Tokio runtime with limited threads
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.worker_threads(args.threads)
		.enable_all()
		.build()?;

	runtime.block_on(async_main(args, labels))
}

async fn async_main(args: Args, labels: HashMap<String, String>) -> Result<()> {
	let env_filter = match args.log_level.to_lowercase().as_str() {
		"error" => format!("docker_stats_exporter=error,tower_http=error"),
		"warn" => format!("docker_stats_exporter=warn,tower_http=warn"),
		"info" => format!("docker_stats_exporter=info,tower_http=info"),
		"debug" => format!("docker_stats_exporter=debug,tower_http=debug,axum::rejection=trace"),
		"trace" => format!("docker_stats_exporter=trace,tower_http=trace,axum::rejection=trace"),
		_ => {
			eprintln!(
				"Invalid log level '{}'. Using 'info' instead.",
				args.log_level
			);
			format!("docker_stats_exporter=info,tower_http=info")
		},
	};

	tracing_subscriber::registry()
		.with(
			tracing_subscriber::EnvFilter::try_from_default_env()
				.unwrap_or_else(|_| env_filter.into()),
		)
		.with(tracing_subscriber::fmt::layer())
		.init();

	tracing::info!("Starting docker stats exporter on {}", args.bind_address);
	if !labels.is_empty() {
		tracing::info!("Using labels: {:?}", labels);
	}

	let app = Router::new()
		.route(
			"/docker-stats/metrics",
			get({
				let labels = labels.clone();
				move || docker_stats_metrics(labels)
			}),
		)
		.layer(TraceLayer::new_for_http());
	let listener = tokio::net::TcpListener::bind(&args.bind_address).await?;
	axum::serve(listener, app).await?;
	Ok(())
}

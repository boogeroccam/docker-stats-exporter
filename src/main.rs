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
	labels: &HashMap<String, String>,
) -> Result<GenericGauge<AtomicF64>> {
	percent_string.pop();
	let value: f64 = percent_string.parse()?;
	get_gauge(name, help, value, labels)
}

fn get_gauge(
	name: String,
	help: String,
	value: f64,
	labels: &HashMap<String, String>,
) -> Result<GenericGauge<AtomicF64>> {
	let mut opts = Opts::new(name.replace("-", "_"), help);

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

fn gauges_for_container(
	stat: &DockerContainerStats,
	labels: &HashMap<String, String>,
) -> Result<Vec<GenericGauge<AtomicF64>>> {
	let cpu_gauge = percent_gauge(
		format!("{}_cpu_usage", stat.container),
		stat.cpu_perc.clone(),
		format!("CPU Usage for the '{}' container", stat.container),
		labels,
	)?;
	let mem_gauge = percent_gauge(
		format!("{}_mem_usage", stat.container),
		stat.mem_perc.clone(),
		format!("MEM Usage for the '{}' container", stat.container),
		labels,
	)?;
	let (input, output) = parse_netio_str(stat.net_io.as_str())?;
	let net_input_gauge = get_gauge(
		format!("{}_network_input_bytes", stat.container),
		format!("Network input bytes for the '{}' container", stat.container),
		input,
		labels,
	)?;
	let net_output_gauge = get_gauge(
		format!("{}_network_output_bytes", stat.container),
		format!(
			"Network output bytes for the '{}' container",
			stat.container
		),
		output,
		labels,
	)?;

	Ok(vec![
		cpu_gauge,
		mem_gauge,
		net_input_gauge,
		net_output_gauge,
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

#[tokio::main]
async fn main() -> Result<()> {
	let args = Args::parse();
	let labels = parse_labels(args.labels)?;

	tracing_subscriber::registry()
		.with(
			tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
				"docker_stats_exporter=debug,tower_http=debug,axum::rejection=trace".into()
			}),
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

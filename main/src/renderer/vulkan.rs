use std::{error::Error, sync::Arc, u32};
use vulkano::{
	device::{Device, DeviceExtensions, Features, Queue},
	format::Format,
	image::{AttachmentImage, ImageCreationError},
	instance::{Instance, PhysicalDevice, QueueFamily},
	swapchain::Surface,
};
use winit::Window;

pub(super) fn create_instance() -> Result<Arc<Instance>, Box<dyn Error>> {
	let mut instance_extensions = vulkano_win::required_extensions();
	instance_extensions.ext_debug_utils = true;

	let mut layers = Vec::new();

	layers.push("VK_LAYER_LUNARG_standard_validation");

	let instance = Instance::new(
		Some(&app_info_from_cargo_toml!()),
		&instance_extensions,
		layers,
	)?;

	Ok(instance)
}

fn find_suitable_physical_device<'a>(
	instance: &'a Arc<Instance>,
	surface: &Surface<Window>,
) -> Result<Option<(PhysicalDevice<'a>, QueueFamily<'a>)>, Box<dyn Error>> {
	for physical_device in PhysicalDevice::enumerate(&instance) {
		let family = {
			let mut val = None;

			for family in physical_device.queue_families() {
				if family.supports_graphics() && surface.is_supported(family)? {
					val = Some(family);
					break;
				}
			}

			val
		};

		if family.is_none() {
			continue;
		}

		let supported_extensions = DeviceExtensions::supported_by_device(physical_device);

		if !supported_extensions.khr_swapchain {
			continue;
		}

		let capabilities = surface.capabilities(physical_device)?;

		if capabilities.supported_formats.is_empty()
			|| capabilities.present_modes.iter().count() == 0
		{
			continue;
		}

		return Ok(Some((physical_device, family.unwrap())));
	}

	Ok(None)
}

pub struct Queues {
	pub graphics: Arc<Queue>,
}

pub(super) fn create_device(
	instance: &Arc<Instance>,
	surface: &Arc<Surface<Window>>,
) -> Result<(Arc<Device>, Queues), Box<dyn Error>> {
	// Select physical device
	let (physical_device, family) = find_suitable_physical_device(&instance, &surface)?
		.ok_or("No suitable physical device found")?;

	let features = Features::none();
	let extensions = DeviceExtensions {
		khr_swapchain: true,
		..DeviceExtensions::none()
	};

	let (device, mut queues) =
		Device::new(physical_device, &features, &extensions, vec![(family, 1.0)])?;

	Ok((
		device,
		Queues {
			graphics: queues.next().unwrap(),
		},
	))
}

pub fn create_depth_buffer(
	device: &Arc<Device>,
	extent: [u32; 2],
) -> Result<Arc<AttachmentImage>, Box<dyn Error>> {
	let allowed_formats = [
		Format::D32Sfloat,
		Format::D32Sfloat_S8Uint,
		Format::D24Unorm_S8Uint,
		Format::D16Unorm,
		Format::D16Unorm_S8Uint,
	];

	for format in allowed_formats.iter().cloned() {
		match AttachmentImage::transient(device.clone(), extent, format) {
			Ok(buf) => return Ok(buf),
			Err(ImageCreationError::FormatNotSupported) => continue,
			Err(any) => return Err(Box::from(any)),
		}
	}

	Err(Box::from("No suitable depth buffer format found."))
}
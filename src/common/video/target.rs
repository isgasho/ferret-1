use anyhow::Context;
use std::sync::Arc;
use vulkano::{
	command_buffer::AutoCommandBufferBuilder,
	device::{Device, DeviceOwned, Queue},
	format::Format,
	image::{swapchain::SwapchainImage, AttachmentImage, ImageAccess, ImageUsage},
	swapchain::{
		AcquireError, ColorSpace, CompositeAlpha, FullscreenExclusive, PresentMode, Surface,
		SurfaceTransform, Swapchain, SwapchainCreationError,
	},
	sync::{FlushError, GpuFuture, SharingMode},
};
use winit::window::Window;

pub struct RenderTarget {
	images: Vec<Arc<SwapchainImage<Window>>>,
	swapchain: Arc<Swapchain<Window>>,
	needs_recreate: bool,
}

impl RenderTarget {
	pub fn new(surface: Arc<Surface<Window>>, device: Arc<Device>) -> anyhow::Result<RenderTarget> {
		let params =
			choose_swapchain_params(&device, &surface, surface.window().inner_size().into())?;
		log::debug!("Creating swapchain: {:?}", params);

		// Create swapchain and images
		let (swapchain, images) = Swapchain::new(
			device.clone(),
			surface,
			params.num_images,
			params.format,
			params.dimensions,
			1,
			ImageUsage {
				transfer_destination: true,
				..ImageUsage::none()
			},
			SharingMode::Exclusive,
			params.transform,
			CompositeAlpha::Opaque,
			params.present_mode,
			FullscreenExclusive::Default,
			true,
			ColorSpace::SrgbNonLinear,
		)
		.context("Couldn't create swapchain")?;

		Ok(RenderTarget {
			images,
			swapchain,
			needs_recreate: false,
		})
	}

	pub fn recreate(&mut self) -> anyhow::Result<()> {
		let params = choose_swapchain_params(
			&self.swapchain.device(),
			self.swapchain.surface(),
			self.swapchain.surface().window().inner_size().into(),
		)?;
		log::debug!("Creating swapchain: {:?}", params);

		let (swapchain, images) = match Swapchain::with_old_swapchain(
			self.swapchain.device().clone(),
			self.swapchain.surface().clone(),
			params.num_images,
			params.format,
			params.dimensions,
			1,
			ImageUsage {
				transfer_destination: true,
				..ImageUsage::none()
			},
			SharingMode::Exclusive,
			params.transform,
			CompositeAlpha::Opaque,
			params.present_mode,
			FullscreenExclusive::Default,
			true,
			ColorSpace::SrgbNonLinear,
			self.swapchain.clone(),
		) {
			Ok(ok) => ok,
			Err(SwapchainCreationError::UnsupportedDimensions) => {
				log::debug!("Swapchain recreation returned UnsupportedDimensions");
				return Ok(());
			}
			Err(err) => Err(err).context("Couldn't recreate swapchain")?,
		};

		*self = RenderTarget {
			images,
			swapchain,
			needs_recreate: false,
		};

		Ok(())
	}

	#[inline]
	pub fn dimensions(&self) -> [u32; 2] {
		self.swapchain.dimensions()
	}

	#[inline]
	pub fn needs_recreate(&self) -> bool {
		self.needs_recreate
	}

	#[inline]
	pub fn window_resized(&mut self, dimensions: [u32; 2]) {
		log::debug!("Window resized to {:?}", dimensions);

		if dimensions != self.dimensions() {
			self.needs_recreate = true;
		}
	}

	pub fn present(
		&mut self,
		queue: &Arc<Queue>,
		image: Arc<AttachmentImage>,
		draw_future: impl GpuFuture,
	) -> anyhow::Result<()> {
		if self.needs_recreate() {
			log::debug!("Swapchain still needs recreating, skipping frame presenting");
			return Ok(());
		}

		// Acquire swapchain image
		let (image_num, suboptimal, swapchain_future) =
			match vulkano::swapchain::acquire_next_image(self.swapchain.clone(), None) {
				Ok(ok) => ok,
				Err(AcquireError::OutOfDate) => {
					self.needs_recreate = true;
					return Ok(());
				}
				Err(x) => Err(x).context("Couldn't acquire swapchain framebuffer")?,
			};

		self.needs_recreate = suboptimal;

		// Blit colour attachment onto swapchain
		let blit_command = {
			let [width, height, depth] = image.dimensions().width_height_depth();
			let mut builder = AutoCommandBufferBuilder::primary_one_time_submit(
				self.swapchain.device().clone(),
				queue.family(),
			)?;
			builder.blit_image(
				image,
				[0, 0, 0],
				[width as i32, height as i32, depth as i32],
				0,
				0,
				self.images[image_num].clone(),
				[0, 0, 0],
				[width as i32, height as i32, depth as i32],
				0,
				0,
				1,
				vulkano::sampler::Filter::Nearest,
			)?;
			builder.build()?
		};

		// Present
		let fence_future = draw_future
			.join(swapchain_future)
			.then_execute(queue.clone(), blit_command)
			.context("Couldn't execute present command")?
			.then_swapchain_present(queue.clone(), self.swapchain.clone(), image_num)
			.then_signal_fence();

		// Wait for fence
		match fence_future.wait(None) {
			Ok(_) => (),
			Err(FlushError::OutOfDate) => self.needs_recreate = true,
			Err(err) => Err(err).context("Couldn't wait for fence")?,
		}

		Ok(())
	}
}

#[derive(Copy, Clone, Debug)]
struct SwapchainParams {
	num_images: u32,
	format: Format,
	dimensions: [u32; 2],
	transform: SurfaceTransform,
	present_mode: PresentMode,
}

fn choose_swapchain_params(
	device: &Arc<Device>,
	surface: &Arc<Surface<Window>>,
	dimensions: [u32; 2],
) -> anyhow::Result<SwapchainParams> {
	let physical_device = device.physical_device();
	let capabilities = surface.capabilities(device.physical_device())?;

	Ok(SwapchainParams {
		num_images: u32::min(
			capabilities.min_image_count + 1,
			capabilities.max_image_count.unwrap_or(std::u32::MAX),
		),
		format: [
			Format::R8G8B8A8Unorm,
			Format::B8G8R8A8Unorm,
			Format::A8B8G8R8UnormPack32,
		]
		.iter()
		.cloned()
		.find(|format| {
			let features = format.properties(physical_device).optimal_tiling_features;
			capabilities
				.supported_formats
				.contains(&(*format, ColorSpace::SrgbNonLinear))
				&& features.blit_dst
		})
		.context("No suitable format found")?,
		dimensions: capabilities.current_extent.unwrap_or(dimensions),
		transform: capabilities.current_transform,
		present_mode: [PresentMode::Mailbox, PresentMode::Fifo]
			.iter()
			.copied()
			.find(|mode| capabilities.present_modes.supports(*mode))
			.context("No suitable present mode found")?,
	})
}

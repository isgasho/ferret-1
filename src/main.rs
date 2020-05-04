mod assets;
mod audio;
mod commands;
mod component;
mod configvars;
mod doom;
mod geometry;
mod input;
mod logger;
mod renderer;
mod stdin;

use crate::{
	assets::{AssetHandle, AssetStorage, DataSource},
	audio::Sound,
	component::EntityTemplate,
	input::{Axis, Bindings, Button, InputState, MouseAxis},
	logger::Logger,
	renderer::{AsBytes, RenderContext},
};
use anyhow::Context;
use nalgebra::{Matrix4, Vector3};
use rand::SeedableRng;
use rand_pcg::Pcg64Mcg;
use rodio::Source;
use shrev::EventChannel;
use specs::{DispatcherBuilder, Entity, ReadExpect, RunNow, World, WorldExt, WriteExpect};
use std::time::{Duration, Instant};
use vulkano::{
	format::Format,
	image::{Dimensions, ImmutableImage},
};
use winit::{
	event::{ElementState, Event, KeyboardInput, MouseButton, VirtualKeyCode, WindowEvent},
	event_loop::{ControlFlow, EventLoop},
	platform::desktop::EventLoopExtDesktop,
};

fn main() -> anyhow::Result<()> {
	Logger::init().unwrap();

	let (command_sender, command_receiver) = crossbeam_channel::unbounded();

	stdin::spawn(command_sender.clone()).context("Could not start stdin thread")?;

	let mut event_loop = EventLoop::new();

	let (render_context, _debug_callback) =
		RenderContext::new(&event_loop).context("Could not create rendering context")?;

	let (sound_sender, sound_receiver) =
		crossbeam_channel::unbounded::<Box<dyn Source<Item = f32> + Send>>();

	std::thread::spawn(move || {
		let device = rodio::default_output_device().unwrap();

		// Play a dummy sound to force the sound engine to initialise itself
		rodio::play_raw(&device, rodio::source::Empty::new());

		for source in sound_receiver {
			rodio::play_raw(&device, source);
		}
	});

	let mut loader = doom::wad::WadLoader::new();
	loader.add("doom.wad").context("Couldn't load doom.wad")?;
	//loader.add("doom.gwa").context("Couldn't load doom.gwa")?;

	let mut bindings = Bindings::new();
	bindings.bind_action(
		doom::input::Action::Attack,
		Button::Mouse(MouseButton::Left),
	);
	bindings.bind_action(doom::input::Action::Use, Button::Key(VirtualKeyCode::Space));
	bindings.bind_action(doom::input::Action::Use, Button::Mouse(MouseButton::Middle));
	bindings.bind_action(
		doom::input::Action::Walk,
		Button::Key(VirtualKeyCode::LShift),
	);
	bindings.bind_action(
		doom::input::Action::Walk,
		Button::Key(VirtualKeyCode::RShift),
	);
	bindings.bind_axis(
		doom::input::Axis::Forward,
		Axis::Emulated {
			pos: Button::Key(VirtualKeyCode::W),
			neg: Button::Key(VirtualKeyCode::S),
		},
	);
	bindings.bind_axis(
		doom::input::Axis::Strafe,
		Axis::Emulated {
			pos: Button::Key(VirtualKeyCode::A),
			neg: Button::Key(VirtualKeyCode::D),
		},
	);
	bindings.bind_axis(
		doom::input::Axis::Yaw,
		Axis::Mouse {
			axis: MouseAxis::X,
			scale: 3.0,
		},
	);
	bindings.bind_axis(
		doom::input::Axis::Pitch,
		Axis::Mouse {
			axis: MouseAxis::Y,
			scale: 3.0,
		},
	);
	//println!("{}", serde_json::to_string(&bindings)?);

	let mut world = World::new();

	// Register components
	world.register::<doom::client::UseAction>();
	world.register::<doom::components::SpawnOnCeiling>();
	world.register::<doom::components::SpawnPoint>();
	world.register::<doom::components::Transform>();
	world.register::<doom::components::Velocity>();
	world.register::<doom::door::DoorActive>();
	world.register::<doom::light::LightFlash>();
	world.register::<doom::light::LightGlow>();
	world.register::<doom::map::LinedefRef>();
	world.register::<doom::map::MapDynamic>();
	world.register::<doom::map::SectorRef>();
	world.register::<doom::physics::BoxCollider>();
	world.register::<doom::render::SpriteRender>();
	world.register::<doom::sound::SoundPlaying>();
	world.register::<doom::update::TextureScroll>();

	// Insert asset storages
	world.insert(AssetStorage::<EntityTemplate>::default());
	world.insert(AssetStorage::<Sound>::default());
	world.insert(AssetStorage::<doom::map::Map>::default());
	world.insert(AssetStorage::<doom::map::textures::Flat>::default());
	world.insert(AssetStorage::<doom::map::textures::WallTexture>::default());
	world.insert(AssetStorage::<doom::image::Palette>::default());
	world.insert(AssetStorage::<doom::sprite::Sprite>::default());
	world.insert(AssetStorage::<doom::sprite::SpriteImage>::default());

	// Insert other resources
	world.insert(Pcg64Mcg::from_entropy());
	world.insert(render_context);
	world.insert(sound_sender);
	world.insert(loader);
	world.insert(InputState::new());
	world.insert(bindings);
	world.insert(Vec::<(AssetHandle<Sound>, Entity)>::new());
	world.insert(doom::client::Client::default());
	world.insert(doom::FRAME_TIME);
	world.insert(EventChannel::<doom::client::UseEvent>::new());

	// Create systems
	let mut render_system =
		doom::render::RenderSystem::new(&world).context("Couldn't create RenderSystem")?;
	let mut sound_system = doom::sound::SoundSystem;
	let mut update_dispatcher = DispatcherBuilder::new()
		.with_thread_local(doom::client::PlayerCommandSystem::default())
		.with_thread_local(doom::client::PlayerMoveSystem::default())
		.with_thread_local(doom::client::PlayerUseSystem::default())
		.with_thread_local(doom::physics::PhysicsSystem::default())
		.with_thread_local(doom::door::DoorUpdateSystem::new(
			world
				.get_mut::<EventChannel<doom::client::UseEvent>>()
				.unwrap()
				.register_reader(),
		))
		.with_thread_local(doom::light::LightUpdateSystem::default())
		.with_thread_local(doom::update::TextureScrollSystem::default())
		.build();

	command_sender.send("map E1M1".to_owned()).ok();

	let mut should_quit = false;
	let mut old_time = Instant::now();
	let mut leftover_time = Duration::default();

	while !should_quit {
		let mut delta;
		let mut new_time;

		// Busy-loop until there is at least a millisecond of delta
		while {
			new_time = Instant::now();
			delta = new_time - old_time;
			delta.as_millis() < 1
		} {}

		old_time = new_time;
		//println!("{} fps", 1.0/delta.as_secs_f32());

		// Process events from the system
		event_loop.run_return(|event, _, control_flow| {
			let (mut input_state, render_context) =
				world.system_data::<(WriteExpect<InputState>, ReadExpect<RenderContext>)>();
			input_state.process_event(&event);

			match event {
				Event::WindowEvent { event, .. } => match event {
					WindowEvent::CloseRequested => {
						command_sender.send("quit".to_owned()).ok();
						*control_flow = ControlFlow::Exit;
					}
					WindowEvent::Resized(_) => {
						if let Err(msg) = render_system.recreate() {
							log::warn!("Error recreating swapchain: {}", msg);
						}
					}
					WindowEvent::MouseInput {
						state: ElementState::Pressed,
						..
					} => {
						let window = render_context.surface().window();
						if let Err(msg) = window.set_cursor_grab(true) {
							log::warn!("Couldn't grab cursor: {}", msg);
						}
						window.set_cursor_visible(false);
						input_state.set_mouse_delta_enabled(true);
					}
					WindowEvent::Focused(false)
					| WindowEvent::KeyboardInput {
						input:
							KeyboardInput {
								state: ElementState::Pressed,
								virtual_keycode: Some(VirtualKeyCode::Escape),
								..
							},
						..
					} => {
						let window = render_context.surface().window();
						if let Err(msg) = window.set_cursor_grab(false) {
							log::warn!("Couldn't release cursor: {}", msg);
						}
						window.set_cursor_visible(true);
						input_state.set_mouse_delta_enabled(false);
					}
					_ => {}
				},
				Event::RedrawEventsCleared => {
					*control_flow = ControlFlow::Exit;
				}
				_ => {}
			}
		});

		// Execute console commands
		while let Some(command) = command_receiver.try_iter().next() {
			// Split into tokens
			let tokens = match commands::tokenize(&command) {
				Ok(tokens) => tokens,
				Err(e) => {
					log::error!("Invalid syntax: {}", e);
					continue;
				}
			};

			// Split further into subcommands
			for args in tokens.split(|tok| tok == ";") {
				match args[0].as_str() {
					"map" => load_map(&args[1], &mut world)?,
					"quit" => should_quit = true,
					_ => log::error!("Unknown command: {}", args[0]),
				}
			}
		}

		if should_quit {
			return Ok(());
		}

		// Run game frames
		leftover_time += delta;

		if leftover_time >= doom::FRAME_TIME {
			leftover_time -= doom::FRAME_TIME;

			update_dispatcher.dispatch(&world);

			// Reset input delta state
			{
				let mut input_state = world.fetch_mut::<InputState>();
				input_state.reset();
			}
		}

		// Update sound
		sound_system.run_now(&world);

		// Draw frame
		render_system.run_now(&world);
	}

	Ok(())
}

fn load_map(name: &str, world: &mut World) -> anyhow::Result<()> {
	log::info!("Starting new game...");
	let start_time = Instant::now();

	// Load palette
	let palette_handle = {
		let (mut loader, mut palette_storage) = world.system_data::<(
			WriteExpect<doom::wad::WadLoader>,
			WriteExpect<AssetStorage<crate::doom::image::Palette>>,
		)>();
		let handle = palette_storage.load("PLAYPAL", &mut *loader);
		palette_storage.build_waiting(Ok);
		handle
	};

	// Load entity type data
	log::info!("Loading entities");
	world.insert(doom::entities::MobjTypes::new(&world));
	world.insert(doom::entities::SectorTypes::new(&world));
	world.insert(doom::entities::LinedefTypes::new(&world));

	// Load sprite images
	{
		let (
			palette_storage,
			mut sprite_storage,
			mut sprite_image_storage,
			mut source,
			render_context,
		) = world.system_data::<(
			ReadExpect<AssetStorage<crate::doom::image::Palette>>,
			WriteExpect<AssetStorage<crate::doom::sprite::Sprite>>,
			WriteExpect<AssetStorage<crate::doom::sprite::SpriteImage>>,
			WriteExpect<crate::doom::wad::WadLoader>,
			ReadExpect<crate::renderer::RenderContext>,
		)>();
		let palette = palette_storage.get(&palette_handle).unwrap();
		sprite_storage.build_waiting(|intermediate| {
			Ok(intermediate.build(&mut *sprite_image_storage, &mut *source)?)
		});

		sprite_image_storage.build_waiting(|image| {
			let data: Vec<_> = image
				.data
				.into_iter()
				.map(|pixel| {
					if pixel.a == 0xFF {
						palette[pixel.i as usize]
					} else {
						crate::doom::image::RGBAColor::default()
					}
				})
				.collect();

			// Create the image
			let matrix = Matrix4::new_translation(&Vector3::new(
				0.0,
				image.offset[0] as f32,
				image.offset[1] as f32,
			)) * Matrix4::new_nonuniform_scaling(&Vector3::new(
				0.0,
				image.size[0] as f32,
				image.size[1] as f32,
			));

			let (image, _future) = ImmutableImage::from_iter(
				data.as_bytes().iter().copied(),
				Dimensions::Dim2d {
					width: image.size[0] as u32,
					height: image.size[1] as u32,
				},
				Format::R8G8B8A8Unorm,
				render_context.queues().graphics.clone(),
			)?;

			Ok(crate::doom::sprite::SpriteImage { matrix, image })
		});
	}

	// Load sounds
	{
		let mut sound_storage = world.system_data::<WriteExpect<AssetStorage<Sound>>>();

		sound_storage.build_waiting(|intermediate| doom::sound::build_sound(intermediate));
	}

	// Load map
	log::info!("Loading map {}...", name);
	let map_handle = {
		let (mut loader, mut map_storage, mut flat_storage, mut wall_texture_storage) = world
			.system_data::<(
				WriteExpect<doom::wad::WadLoader>,
				WriteExpect<AssetStorage<doom::map::Map>>,
				WriteExpect<AssetStorage<doom::map::textures::Flat>>,
				WriteExpect<AssetStorage<doom::map::textures::WallTexture>>,
			)>();
		let map_handle = map_storage.load(name, &mut *loader);
		map_storage.build_waiting(|data| {
			doom::map::load::build_map(
				data,
				"SKY1",
				&mut *loader,
				&mut *flat_storage,
				&mut *wall_texture_storage,
			)
		});

		map_handle
	};

	// Build flats and wall textures
	{
		let (palette_storage, mut flat_storage, render_context) = world.system_data::<(
			ReadExpect<AssetStorage<doom::image::Palette>>,
			WriteExpect<AssetStorage<doom::map::textures::Flat>>,
			ReadExpect<RenderContext>,
		)>();
		let palette = palette_storage.get(&palette_handle).unwrap();
		flat_storage.build_waiting(|image| {
			let data: Vec<_> = image
				.data
				.into_iter()
				.map(|pixel| {
					if pixel.a == 0xFF {
						palette[pixel.i as usize]
					} else {
						crate::doom::image::RGBAColor::default()
					}
				})
				.collect();

			// Create the image
			let (image, _future) = ImmutableImage::from_iter(
				data.as_bytes().iter().copied(),
				Dimensions::Dim2d {
					width: image.size[0] as u32,
					height: image.size[1] as u32,
				},
				Format::R8G8B8A8Unorm,
				render_context.queues().graphics.clone(),
			)?;

			Ok(image)
		});
	}

	{
		let (palette_storage, mut wall_texture_storage, render_context) = world.system_data::<(
			ReadExpect<AssetStorage<doom::image::Palette>>,
			WriteExpect<AssetStorage<doom::map::textures::WallTexture>>,
			ReadExpect<RenderContext>,
		)>();
		let palette = palette_storage.get(&palette_handle).unwrap();
		wall_texture_storage.build_waiting(|image| {
			let data: Vec<_> = image
				.data
				.into_iter()
				.map(|pixel| {
					if pixel.a == 0xFF {
						palette[pixel.i as usize]
					} else {
						crate::doom::image::RGBAColor::default()
					}
				})
				.collect();

			let (image, _future) = ImmutableImage::from_iter(
				data.as_bytes().iter().copied(),
				Dimensions::Dim2d {
					width: image.size[0] as u32,
					height: image.size[1] as u32,
				},
				Format::R8G8B8A8Unorm,
				render_context.queues().graphics.clone(),
			)?;

			Ok(image)
		});
	}

	// Spawn map entities and things
	let things = {
		let loader = world.system_data::<WriteExpect<doom::wad::WadLoader>>();
		doom::map::load::build_things(&loader.load(&format!("{}/+{}", name, 1))?)?
	};
	doom::map::spawn_map_entities(&world, &map_handle)?;
	doom::map::spawn_things(things, &world, &map_handle)?;

	// Spawn player
	let entity = doom::map::spawn_player(&world)?;
	world
		.system_data::<WriteExpect<doom::client::Client>>()
		.entity = Some(entity);

	log::debug!(
		"Loading took {} s",
		(Instant::now() - start_time).as_secs_f32()
	);

	Ok(())
}

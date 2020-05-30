use crate::{
	assets::{AssetHandle, AssetStorage},
	audio::Sound,
	doom::{
		client::{UseAction, UseEvent},
		map::{LinedefRef, Map, MapDynamic, SectorRef},
		physics::{SectorTracer, TouchAction, TouchEvent},
		switch::{SwitchActive, SwitchParams},
	},
};
use legion::prelude::{
	CommandBuffer, Entity, IntoQuery, Read, ResourceSet, Resources, World, Write,
};
use shrev::EventChannel;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct FloorActive {
	pub speed: f32,
	pub target_height: f32,
	pub move_sound: AssetHandle<Sound>,
	pub move_sound_time: Duration,
	pub move_sound_time_left: Duration,
	pub finish_sound: AssetHandle<Sound>,
}

#[derive(Clone, Debug)]
pub struct FloorParams {
	pub speed: f32,
	pub target_height_base: TargetHeight,
	pub target_height_offset: f32,
	pub move_sound: AssetHandle<Sound>,
	pub move_sound_time: Duration,
	pub finish_sound: AssetHandle<Sound>,
}

#[derive(Clone, Copy, Debug)]
pub enum TargetHeight {
	Current,
	LowestNeighbourFloor,
	LowestNeighbourFloorAbove,
	LowestNeighbourCeiling,
	HighestNeighbourFloor,
}

pub fn floor_active_system() -> Box<dyn FnMut(&mut World, &mut Resources)> {
	Box::new(move |world, resources| {
		let (asset_storage, delta, mut sound_queue) = <(
			Read<AssetStorage>,
			Read<Duration>,
			Write<Vec<(AssetHandle<Sound>, Entity)>>,
		)>::fetch_mut(resources);

		let tracer = SectorTracer { world };
		let mut command_buffer = CommandBuffer::new(world);

		for (entity, (sector_ref, mut floor_active)) in unsafe {
			<(Read<SectorRef>, Write<FloorActive>)>::query().iter_entities_unchecked(world)
		} {
			let mut map_dynamic = unsafe {
				world
					.get_component_mut_unchecked::<MapDynamic>(sector_ref.map_entity)
					.unwrap()
			};
			let map = asset_storage.get(&map_dynamic.map).unwrap();
			let sector = &map.sectors[sector_ref.index];
			let sector_dynamic = &mut map_dynamic.sectors[sector_ref.index];

			if let Some(new_time) = floor_active.move_sound_time_left.checked_sub(*delta) {
				floor_active.move_sound_time_left = new_time;
			} else {
				floor_active.move_sound_time_left = floor_active.move_sound_time;
				sound_queue.push((floor_active.move_sound.clone(), entity));
			}

			let done = {
				let direction = if floor_active.target_height < sector_dynamic.interval.min {
					-1.0
				} else {
					1.0
				};

				let move_step = direction * floor_active.speed * delta.as_secs_f32();
				let trace = tracer.trace(
					sector_dynamic.interval.min,
					1.0,
					move_step,
					sector.subsectors.iter().map(|i| &map.subsectors[*i]),
				);

				if trace.collision.is_some() {
					// Hang there until the obstruction is gone
					false
				} else {
					sector_dynamic.interval.min += move_step;

					if direction * sector_dynamic.interval.min
						>= direction * floor_active.target_height
					{
						sector_dynamic.interval.min = floor_active.target_height;
						true
					} else {
						false
					}
				}
			};

			if done {
				sound_queue.push((floor_active.finish_sound.clone(), entity));
				command_buffer.remove_component::<FloorActive>(entity);
			}
		}

		command_buffer.write(world);
	})
}

#[derive(Clone, Debug)]
pub struct FloorSwitchUse {
	pub params: FloorParams,
	pub switch_params: SwitchParams,
}

pub fn floor_switch_system(
	resources: &mut Resources,
) -> Box<dyn FnMut(&mut World, &mut Resources)> {
	let mut use_event_reader = resources
		.get_mut::<EventChannel<UseEvent>>()
		.unwrap()
		.register_reader();

	Box::new(move |world, resources| {
		let (asset_storage, use_event_channel, mut sound_queue) = <(
			Read<AssetStorage>,
			Read<EventChannel<UseEvent>>,
			Write<Vec<(AssetHandle<Sound>, Entity)>>,
		)>::fetch_mut(resources);

		let mut command_buffer = CommandBuffer::new(world);

		for use_event in use_event_channel.read(&mut use_event_reader) {
			let linedef_ref = world
				.get_component::<LinedefRef>(use_event.linedef_entity)
				.unwrap();
			let mut map_dynamic = unsafe {
				world
					.get_component_mut_unchecked::<MapDynamic>(linedef_ref.map_entity)
					.unwrap()
			};
			let map = asset_storage.get(&map_dynamic.map).unwrap();
			let linedef = &map.linedefs[linedef_ref.index];

			if let Some(UseAction::FloorSwitchUse(floor_use)) = world
				.get_component::<UseAction>(use_event.linedef_entity)
				.as_deref()
			{
				// Skip if switch is already in active state
				if world.has_component::<SwitchActive>(use_event.linedef_entity) {
					continue;
				}

				let activated = activate_with_tag(
					&floor_use.params,
					&mut command_buffer,
					linedef.sector_tag,
					world,
					map,
					map_dynamic.as_ref(),
				);

				if activated {
					let activated = crate::doom::switch::activate(
						&floor_use.switch_params,
						&mut command_buffer,
						sound_queue.as_mut(),
						linedef_ref.index,
						map,
						map_dynamic.as_mut(),
					);

					if activated && floor_use.switch_params.retrigger_time.is_none() {
						command_buffer.remove_component::<UseAction>(use_event.linedef_entity);
					}
				}
			}
		}

		command_buffer.write(world);
	})
}

#[derive(Clone, Debug)]
pub struct FloorTouch {
	pub params: FloorParams,
	pub retrigger: bool,
}

pub fn floor_touch_system(resources: &mut Resources) -> Box<dyn FnMut(&mut World, &mut Resources)> {
	let mut touch_event_reader = resources
		.get_mut::<EventChannel<TouchEvent>>()
		.unwrap()
		.register_reader();

	Box::new(move |world, resources| {
		let (asset_storage, touch_event_channel) =
			<(Read<AssetStorage>, Read<EventChannel<TouchEvent>>)>::fetch(resources);

		let mut command_buffer = CommandBuffer::new(world);

		for touch_event in touch_event_channel.read(&mut touch_event_reader) {
			if touch_event.collision.is_some() {
				continue;
			}

			let linedef_ref =
				if let Some(linedef_ref) = world.get_component::<LinedefRef>(touch_event.touched) {
					linedef_ref
				} else {
					continue;
				};
			let map_dynamic = unsafe {
				world
					.get_component_mut_unchecked::<MapDynamic>(linedef_ref.map_entity)
					.unwrap()
			};
			let map = asset_storage.get(&map_dynamic.map).unwrap();
			let linedef = &map.linedefs[linedef_ref.index];

			match world
				.get_component::<TouchAction>(touch_event.touched)
				.as_deref()
			{
				Some(TouchAction::FloorTouch(floor_touch)) => {
					if activate_with_tag(
						&floor_touch.params,
						&mut command_buffer,
						linedef.sector_tag,
						world,
						map,
						map_dynamic.as_ref(),
					) {
						if !floor_touch.retrigger {
							command_buffer.remove_component::<TouchAction>(touch_event.touched);
						}
					}
				}
				_ => {}
			}
		}

		command_buffer.write(world);
	})
}

fn activate(
	params: &FloorParams,
	command_buffer: &mut CommandBuffer,
	sector_index: usize,
	map: &Map,
	map_dynamic: &MapDynamic,
) {
	let sector_dynamic = &map_dynamic.sectors[sector_index];

	let target_height = match params.target_height_base {
		TargetHeight::Current => sector_dynamic.interval.min + params.target_height_offset,
		TargetHeight::LowestNeighbourFloor => {
			map.lowest_neighbour_floor(map_dynamic, sector_index) + params.target_height_offset
		}
		TargetHeight::LowestNeighbourFloorAbove => {
			map.lowest_neighbour_floor_above(map_dynamic, sector_index, sector_dynamic.interval.min)
				+ params.target_height_offset
		}
		TargetHeight::LowestNeighbourCeiling => {
			let mut target_height = map.lowest_neighbour_ceiling(map_dynamic, sector_index);

			if target_height > sector_dynamic.interval.min {
				target_height = sector_dynamic.interval.min;
			}

			target_height + params.target_height_offset
		}
		TargetHeight::HighestNeighbourFloor => {
			let target_height = map.highest_neighbour_floor(map_dynamic, sector_index);

			if target_height != sector_dynamic.interval.min {
				target_height + params.target_height_offset
			} else {
				target_height
			}
		}
	};

	command_buffer.add_component(
		sector_dynamic.entity,
		FloorActive {
			speed: params.speed,
			target_height,
			move_sound: params.move_sound.clone(),
			move_sound_time: params.move_sound_time,
			move_sound_time_left: Duration::default(),
			finish_sound: params.finish_sound.clone(),
		},
	);
}

fn activate_with_tag(
	params: &FloorParams,
	command_buffer: &mut CommandBuffer,
	sector_tag: u16,
	world: &World,
	map: &Map,
	map_dynamic: &MapDynamic,
) -> bool {
	let mut activated = false;

	// Activate all the doors with the same tag
	for (sector_index, _) in map
		.sectors
		.iter()
		.enumerate()
		.filter(|(_, s)| s.sector_tag == sector_tag)
	{
		let sector_entity = map_dynamic.sectors[sector_index].entity;

		if world.has_component::<FloorActive>(sector_entity) {
			continue;
		}

		activated = true;
		activate(params, command_buffer, sector_index, map, map_dynamic);
	}

	activated
}
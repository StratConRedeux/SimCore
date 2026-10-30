use super::*;
use simcore_abi::flags;

#[test]
fn null_handles_are_safe() {
    // SAFETY: NULL is an accepted input for every handle-taking function.
    unsafe {
        sc_world_destroy(std::ptr::null_mut());
        sc_world_step(std::ptr::null_mut(), 10);
        assert_eq!(sc_world_tick(std::ptr::null()), 0);
        let f = sc_world_frame(std::ptr::null_mut());
        assert!(f.units.is_null());
        assert_eq!((f.count, f.tick), (0, 0));
        let e = sc_world_engagements(std::ptr::null_mut());
        assert!(e.events.is_null());
        assert_eq!(e.count, 0);
        let l = sc_world_launches(std::ptr::null_mut());
        assert!(l.launches.is_null());
        assert_eq!(l.count, 0);
        let s = sc_world_standing_orders(std::ptr::null_mut());
        assert!(s.orders.is_null());
        assert_eq!(s.count, 0);
        assert_eq!(
            sc_world_add_standing_order(std::ptr::null_mut(), std::ptr::null()),
            0
        );
        assert!(!sc_world_cancel_standing_order(std::ptr::null_mut(), 1));
        assert_eq!(
            sc_world_standing_order_stock(std::ptr::null_mut(), 1),
            u32::MAX
        );
    }
}

#[test]
fn frame_is_engine_buffer_without_copy() {
    let w = sc_world_create_dummy(7, 1000);
    assert!(!w.is_null());
    // SAFETY: `w` is a live handle used only on this thread, destroyed once at the end;
    // `f.units` is read only before the next mutating call.
    unsafe {
        sc_world_step(w, 3);
        assert_eq!(sc_world_tick(w), 3);
        let f = sc_world_frame(w);
        assert_eq!(f.count, 1001);
        assert_eq!(f.tick, 3);
        let units = std::slice::from_raw_parts(f.units, f.count as usize);
        let cmd: Vec<_> = units
            .iter()
            .filter(|u| u.flags & flags::COMMAND_NODE != 0)
            .collect();
        assert_eq!(cmd.len(), 1);
        assert_eq!((cmd[0].x, cmd[0].y, cmd[0].z), (0.0, 0.0, 0.0));
        // Done reading `units`. The engine buffer is reused, so a rebuilt frame (through the
        // engine or the C API) comes back at the same address: no copy is made.
        assert_eq!(
            (*w).sim.frame().as_ptr(),
            f.units,
            "points into the engine buffer"
        );
        assert_eq!(sc_world_frame(w).units, f.units);
        sc_world_destroy(w);
    }
}

#[test]
fn unrepresentable_unit_count_is_null() {
    assert!(sc_world_create_dummy(1, u32::MAX).is_null());
    // More units than ids: the world's snapshot could never be restored.
    assert!(sc_world_create_dummy(1, MAX_DUMMY_UNITS + 1).is_null());
}

#[test]
fn new_calls_accept_null() {
    // SAFETY: NULL is an accepted input for every handle and pointer argument.
    unsafe {
        sc_world_despawn_removed(std::ptr::null_mut());
        assert_eq!(sc_world_status(std::ptr::null_mut()), ScStatus::default());
        assert_eq!(sc_world_link_delay(std::ptr::null_mut(), 1), u32::MAX);
        let c = ScCommand::default();
        assert_eq!(sc_world_command(std::ptr::null_mut(), &c), 0);
        let w = sc_world_create(1);
        assert_eq!(sc_world_command(w, std::ptr::null()), 0);
        let b = sc_world_save(std::ptr::null_mut());
        assert!(b.data.is_null() && b.len == 0);
        assert!(sc_world_restore(std::ptr::null(), 10).is_null());
        assert!(sc_world_restore(b"SCSN".as_ptr(), 4).is_null());
        sc_world_destroy(w);
    }
}

#[test]
fn command_status_save_restore() {
    let w = sc_world_create_dummy(3, 300);
    // SAFETY: `w` and `r` are live handles used only on this thread, each destroyed once;
    // `saved` is copied out of the engine buffer before the next call on `w`.
    unsafe {
        let units = {
            let f = sc_world_frame(w);
            std::slice::from_raw_parts(f.units, f.count as usize).to_vec()
        };
        let friendly = units
            .iter()
            .find(|u| u.affiliation == simcore_abi::affiliation::FRIENDLY && u.id != 1)
            .unwrap();
        let hostile = units
            .iter()
            .find(|u| u.affiliation == simcore_abi::affiliation::HOSTILE)
            .unwrap();
        let mv = ScCommand {
            unit_id: friendly.id,
            x: friendly.x + 30.0,
            y: friendly.y,
            z: friendly.z,
            speed_mps: 10.0,
            type_: command::MOVE,
            ..ScCommand::default()
        };
        assert_eq!(sc_world_link_delay(w, friendly.id), 0);
        assert_eq!(sc_world_link_delay(w, 99_999), u32::MAX);
        assert_eq!(sc_world_command(w, &mv), 1);
        for bad in [
            ScCommand {
                unit_id: hostile.id,
                ..mv
            },
            ScCommand {
                _reserved1: 1,
                ..mv
            },
            ScCommand {
                speed_mps: 0.0,
                ..mv
            },
            ScCommand { type_: 99, ..mv },
            ScCommand {
                type_: command::EMISSIONS,
                radiate: 2,
                ..mv
            },
        ] {
            assert_eq!(sc_world_command(w, &bad), 0, "{bad:?}");
        }
        assert_eq!(sc_world_status(w).pending_commands, 1);

        let b = sc_world_save(w);
        assert!(!b.data.is_null() && b.len > 0);
        let saved = std::slice::from_raw_parts(b.data, b.len as usize).to_vec();
        let r = sc_world_restore(saved.as_ptr(), saved.len() as u64);
        assert!(!r.is_null());
        assert_eq!(sc_world_status(r), sc_world_status(w));

        // Both deliver the move and travel the same 30 m: 45 ticks at 10 m/s.
        sc_world_step(w, 60);
        sc_world_step(r, 60);
        let pos = |h| {
            let f = sc_world_frame(h);
            let u = std::slice::from_raw_parts(f.units, f.count as usize);
            let u = u.iter().find(|u| u.id == friendly.id).unwrap();
            (u.x, u.y, u.vx)
        };
        let (pw, pr) = (pos(w), pos(r));
        assert_eq!(pw, pr);
        assert!(
            (pw.0 - mv.x).abs() < 0.5 && pw.2 == 0.0,
            "arrived and stopped: {pw:?}"
        );
        assert_eq!(sc_world_status(w), sc_world_status(r));
        sc_world_despawn_removed(w);
        sc_world_destroy(w);
        sc_world_destroy(r);
    }
}

fn unit_at(x: f64, y: f64, aff: u8, kind: u16) -> ScUnitSpec {
    ScUnitSpec {
        x,
        y,
        erp_w: 100.0,
        kind,
        affiliation: aff,
        ..ScUnitSpec::default()
    }
}

fn engagements_of(w: *mut ScWorld) -> Vec<ScEngagement> {
    // SAFETY: `w` is a live handle on this thread; the events are copied out before any
    // other call on it, and only read when there are some (NULL otherwise).
    unsafe {
        let e = sc_world_engagements(w);
        if e.count == 0 {
            assert!(e.events.is_null());
            return Vec::new();
        }
        std::slice::from_raw_parts(e.events, e.count as usize).to_vec()
    }
}

fn frame_of(w: *mut ScWorld) -> Vec<ScUnitView> {
    // SAFETY: `w` is a live handle on this thread; the units are copied out before any
    // other call on it.
    unsafe {
        let f = sc_world_frame(w);
        let mut v = std::slice::from_raw_parts(f.units, f.count as usize).to_vec();
        v.sort_by_key(|u| u.id);
        v
    }
}

#[test]
fn setup_calls_reject_bad_input_and_change_nothing() {
    use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
    use simcore_engine::kind;
    let w = sc_world_create(1);
    // SAFETY: `w` is a live handle used only on this thread and destroyed once; every
    // pointer passed points to a live local of the right type and count.
    unsafe {
        let null = std::ptr::null_mut();
        let good_rules = ScRules {
            detection_max_range_m: 5000.0,
            default_chance: 0.5,
            ..ScRules::default()
        };
        assert!(!sc_world_set_rules(null, &good_rules, std::ptr::null(), 0));
        assert!(!sc_world_set_rules(
            w,
            std::ptr::null(),
            std::ptr::null(),
            0
        ));
        assert!(!sc_world_set_rules(w, &good_rules, std::ptr::null(), 1));
        for bad in [
            ScRules {
                _reserved: 1,
                ..good_rules
            },
            ScRules {
                default_chance: 1.5,
                ..good_rules
            },
            ScRules {
                detection_max_range_m: -1.0,
                ..good_rules
            },
            ScRules {
                sensor_base_range_m: f64::INFINITY,
                ..good_rules
            },
            ScRules {
                sensor_reference_power_w: f64::NAN,
                ..good_rules
            },
        ] {
            assert!(!sc_world_set_rules(w, &bad, std::ptr::null(), 0), "{bad:?}");
        }
        let o = ScOutcome {
            outcome_class: 1,
            target_kind: 4,
            _reserved: 0,
            chance: 1.0,
        };
        for bad in [
            [o, o],
            [
                o,
                ScOutcome {
                    chance: -0.1,
                    target_kind: 5,
                    ..o
                },
            ],
            [
                o,
                ScOutcome {
                    _reserved: 1,
                    target_kind: 5,
                    ..o
                },
            ],
        ] {
            assert!(
                !sc_world_set_rules(w, &good_rules, bad.as_ptr(), 2),
                "{bad:?}"
            );
        }
        assert_eq!(
            (*w).sim.rules(),
            &Rules::default(),
            "rejections change nothing"
        );
        let two = [
            o,
            ScOutcome {
                target_kind: 5,
                chance: 0.0,
                ..o
            },
        ];
        assert!(sc_world_set_rules(w, &good_rules, two.as_ptr(), 2));
        let r = (*w).sim.rules();
        assert_eq!(r.outcomes.chance(1, 4), 1.0);
        assert_eq!(r.outcomes.chance(1, 5), 0.0);
        assert_eq!(r.outcomes.chance(9, 9), 0.5);

        let link = ScLinkRules {
            base_delay_ticks: 3,
            max_delay_ticks: 100,
            ..ScLinkRules::default()
        };
        assert!(!sc_world_set_link_rules(w, std::ptr::null()));
        assert!(!sc_world_set_link_rules(
            w,
            &ScLinkRules {
                jam_radius_m: -1.0,
                ..link
            }
        ));
        assert!(!sc_world_set_link_rules(
            w,
            &ScLinkRules {
                delay_ticks_per_km: f64::NAN,
                ..link
            }
        ));
        assert!(!sc_world_set_link_rules(
            w,
            &ScLinkRules {
                _reserved: 7,
                ..link
            }
        ));
        assert_eq!(sc_world_link_delay(w, 1), 0);
        assert!(sc_world_set_link_rules(w, &link));
        assert_eq!(sc_world_link_delay(w, 1), 3);

        let t = ScTerrain {
            spacing_m: 100.0,
            nx: 2,
            ny: 2,
            los_samples: 8,
            ..ScTerrain::default()
        };
        let h = [0.0f32, 10.0, 20.0, 30.0];
        assert!(!sc_world_set_terrain(w, &t, std::ptr::null(), 4));
        assert!(
            !sc_world_set_terrain(w, &t, h.as_ptr(), 3),
            "count must be nx * ny"
        );
        // A wrong count is refused before a single height is read.
        assert!(!sc_world_set_terrain(
            w,
            &t,
            h.as_ptr(),
            u64::from(u32::MAX)
        ));
        assert!(!sc_world_set_terrain(
            w,
            &ScTerrain {
                los_samples: 1025,
                ..t
            },
            h.as_ptr(),
            4
        ));
        assert!(!sc_world_set_terrain(
            w,
            &ScTerrain { _reserved: 1, ..t },
            h.as_ptr(),
            4
        ));
        let nan = [0.0f32, f32::NAN, 0.0, 0.0];
        assert!(!sc_world_set_terrain(w, &t, nan.as_ptr(), 4));
        assert!((*w).sim.terrain().is_flat());
        assert!(sc_world_set_terrain(w, &t, h.as_ptr(), 4));
        assert_eq!((*w).sim.terrain().height_at(100.0, 100.0), 30.0);
        assert!(sc_world_set_terrain(
            w,
            std::ptr::null(),
            std::ptr::null(),
            0
        ));
        assert!((*w).sim.terrain().is_flat());

        let radar = unit_at(500.0, 0.0, HOSTILE, kind::GROUND_RADAR);
        let strike = ScUnitSpec {
            options: spawn::PURSUER,
            target_id: 2,
            speed_mps: 50.0,
            max_turn_rate_rad_s: 1.0,
            engagement_radius_m: 5.0,
            ..unit_at(0.0, 0.0, FRIENDLY, kind::FIXED_WING_DRONE)
        };
        assert_eq!(sc_world_spawn(null, &radar), 0);
        assert_eq!(sc_world_spawn(w, std::ptr::null()), 0);
        assert_eq!(sc_world_spawn(w, &strike), 0, "target 2 does not exist yet");
        for bad in [
            ScUnitSpec {
                x: f64::NAN,
                ..radar
            },
            ScUnitSpec {
                vz: f64::INFINITY,
                ..radar
            },
            ScUnitSpec {
                erp_w: -1.0,
                ..radar
            },
            ScUnitSpec {
                affiliation: 4,
                ..radar
            },
            ScUnitSpec {
                kind: kind::COMMAND_NODE,
                ..radar
            },
            ScUnitSpec {
                options: 1 << 4,
                ..radar
            },
            ScUnitSpec {
                options: spawn::EMITTING | spawn::CYCLE,
                ..radar
            },
            ScUnitSpec {
                _reserved: 1,
                ..radar
            },
        ] {
            assert_eq!(sc_world_spawn(w, &bad), 0, "{bad:?}");
        }
        assert_eq!(sc_world_status(w).unit_count, 1, "rejections spawn nothing");
        assert_eq!(sc_world_spawn(w, &radar), 2);
        for bad in [
            ScUnitSpec {
                speed_mps: 0.0,
                ..strike
            },
            ScUnitSpec {
                lead: 1.5,
                ..strike
            },
            ScUnitSpec {
                max_turn_rate_rad_s: -1.0,
                ..strike
            },
            ScUnitSpec {
                engagement_radius_m: f64::NAN,
                ..strike
            },
            ScUnitSpec {
                target_id: 99,
                ..strike
            },
        ] {
            assert_eq!(sc_world_spawn(w, &bad), 0, "{bad:?}");
        }
        assert_eq!(
            sc_world_spawn(w, &strike),
            3,
            "ids are not used up by rejections"
        );

        let s = ScSensor {
            base_range_m: 1000.0,
            reference_power_w: 1.0,
        };
        assert!(!sc_world_add_sensor(null, 1, &s));
        assert!(!sc_world_add_sensor(w, 99, &s));
        assert!(!sc_world_add_sensor(
            w,
            1,
            &ScSensor {
                base_range_m: -1.0,
                ..s
            }
        ));
        assert!(sc_world_add_sensor(w, 1, &s));
        assert!(sc_world_add_sensor(w, 2, std::ptr::null()));
        sc_world_destroy(w);
    }
}

#[test]
fn scenario_built_over_the_c_api_plays_and_survives_save_restore() {
    use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
    let w = sc_world_create(42);
    // SAFETY: `w` and `r` are live handles used only on this thread, each destroyed once;
    // pointers point to live locals; snapshot bytes are copied before the next call on `w`.
    unsafe {
        let rules = ScRules {
            detection_max_range_m: 20_000.0,
            sensor_base_range_m: 2_000.0,
            sensor_reference_power_w: 1.0,
            default_chance: 0.0,
            exposure_threshold_ticks: 3,
            exposure_cap_ticks: 30,
            exposure_decay_per_tick: 1,
            _reserved: 0,
        };
        let hit = [ScOutcome {
            outcome_class: 7,
            target_kind: kind::GROUND_RADAR,
            _reserved: 0,
            chance: 1.0,
        }];
        assert!(sc_world_set_rules(w, &rules, hit.as_ptr(), 1));
        let link = ScLinkRules {
            base_delay_ticks: 2,
            delay_ticks_per_km: 1.0,
            max_delay_ticks: 60,
            ..ScLinkRules::default()
        };
        assert!(sc_world_set_link_rules(w, &link));
        assert!(sc_world_add_sensor(w, 1, std::ptr::null()));
        // A hostile radar 3 km east, always on: 50 kW is heard out to 2 km * sqrt(50000).
        let radar = sc_world_spawn(
            w,
            &ScUnitSpec {
                erp_w: 50_000.0,
                options: spawn::EMITTING,
                ..unit_at(3_000.0, 0.0, HOSTILE, kind::GROUND_RADAR)
            },
        );
        // A friendly quadcopter on a duty cycle, with its own sensor.
        let drone = sc_world_spawn(
            w,
            &ScUnitSpec {
                z: 100.0,
                erp_w: 1.0,
                options: spawn::CYCLE | spawn::SENSOR,
                cycle_on_ticks: 5,
                cycle_off_ticks: 10,
                ..unit_at(-200.0, 0.0, FRIENDLY, kind::QUADCOPTER)
            },
        );
        // A strike on the radar, launched eastward from the node.
        let strike = sc_world_spawn(
            w,
            &ScUnitSpec {
                vx: 1.0,
                options: spawn::PURSUER,
                target_id: radar,
                outcome_class: 7,
                speed_mps: 100.0,
                max_turn_rate_rad_s: 1.0,
                engagement_radius_m: 10.0,
                ..unit_at(0.0, 0.0, FRIENDLY, kind::FIXED_WING_DRONE)
            },
        );
        assert_eq!((radar, drone, strike), (2, 3, 4));
        let f = frame_of(w);
        assert_eq!(f.len(), 4);
        assert_eq!(
            (f[1].x, f[1].kind, f[1].affiliation),
            (3_000.0, kind::GROUND_RADAR, HOSTILE)
        );
        assert!(f[1].flags & simcore_abi::flags::EMITTING != 0);
        assert!(
            f[2].flags & simcore_abi::flags::EMITTING != 0,
            "cycle on at tick 0"
        );
        // 2 base + ceil(3 km * 1/km).
        assert_eq!(sc_world_link_delay(w, radar), 5);
        assert!(
            sc_world_command(
                w,
                &ScCommand {
                    unit_id: drone,
                    x: -200.0,
                    y: 300.0,
                    z: 100.0,
                    speed_mps: 10.0,
                    type_: command::MOVE,
                    ..ScCommand::default()
                }
            ) > 0
        );

        sc_world_step(w, 10);
        assert_eq!(
            sc_world_status(w).hostile_detected,
            1,
            "the node hears the radar"
        );
        let saved = {
            let b = sc_world_save(w);
            std::slice::from_raw_parts(b.data, b.len as usize).to_vec()
        };
        let r = sc_world_restore(saved.as_ptr(), saved.len() as u64);
        assert!(!r.is_null());
        assert!(engagements_of(w).is_empty(), "nothing has engaged yet");
        // 3 km at 100 m/s: about 30 s, 450 ticks. Both worlds play it identically.
        let mut events = Vec::new();
        for _ in 0..60 {
            sc_world_step(w, 10);
            sc_world_step(r, 10);
            assert_eq!(frame_of(w), frame_of(r));
            assert_eq!(sc_world_status(w), sc_world_status(r));
            assert_eq!(engagements_of(w), engagements_of(r));
            events.extend(engagements_of(w));
        }
        // The strike's one engagement: a kill, reported where the radar stood.
        let [e] = events[..] else {
            panic!("one engagement: {events:?}");
        };
        assert_eq!(
            (
                e.pursuer_id,
                e.target_id,
                e.target_kind,
                e.target_affiliation
            ),
            (strike, radar, kind::GROUND_RADAR, HOSTILE)
        );
        assert_eq!(e.flags, engagement::SUCCESS | engagement::TARGET_REMOVED);
        assert_eq!((e.x, e.y, e.z), (3_000.0, 0.0, 0.0));
        assert!((400..=500).contains(&e.tick), "engaged at tick {}", e.tick);
        let s = sc_world_status(w);
        assert_eq!(
            s.unit_count, 2,
            "the strike removed the radar and itself: {s:?}"
        );
        let ids: Vec<u64> = frame_of(w).iter().map(|u| u.id).collect();
        assert_eq!(ids, [1, drone]);
        let d = frame_of(w)[1];
        assert!(
            (d.y - 300.0).abs() < 0.5 && d.vy == 0.0,
            "the drone arrived: {d:?}"
        );
        // Ids are never reused.
        assert_eq!(
            sc_world_spawn(w, &unit_at(0.0, 0.0, FRIENDLY, kind::QUADCOPTER)),
            5
        );
        sc_world_destroy(w);
        sc_world_destroy(r);
    }
}

#[test]
fn setup_coordinates_are_relative_to_the_command_node() {
    let w = sc_world_create(1);
    // SAFETY: `w` is a live handle used only on this thread and destroyed once.
    unsafe {
        (*w).sim.set_command_node_position(Position {
            x: 1e6,
            y: -2e6,
            z: 50.0,
        });
        let id = sc_world_spawn(w, &unit_at(10.0, 20.0, 1, simcore_engine::kind::QUADCOPTER));
        let p = (*w).sim.position(StableId(id)).unwrap();
        assert_eq!((p.x, p.y, p.z), (1e6 + 10.0, -2e6 + 20.0, 50.0));
        let t = ScTerrain {
            origin_x: -5.0,
            origin_y: 5.0,
            spacing_m: 1.0,
            nx: 1,
            ny: 1,
            ..ScTerrain::default()
        };
        assert!(sc_world_set_terrain(w, &t, [7.0f32].as_ptr(), 1));
        let (ox, oy, ..) = (*w).sim.terrain().parts().unwrap();
        assert_eq!((ox, oy), (1e6 - 5.0, -2e6 + 5.0));
        sc_world_destroy(w);
    }
}

fn launches_of(w: *mut ScWorld) -> Vec<ScLaunch> {
    // SAFETY: `w` is a live handle on this thread; the launches are copied out before any
    // other call on it, and only read when there are some (NULL otherwise).
    unsafe {
        let l = sc_world_launches(w);
        if l.count == 0 {
            assert!(l.launches.is_null());
            return Vec::new();
        }
        std::slice::from_raw_parts(l.launches, l.count as usize).to_vec()
    }
}

fn sop(x: f64, y: f64) -> ScStandingOrder {
    ScStandingOrder {
        x,
        y,
        radius_m: 300.0,
        kinds: 0,
        stock: 3,
        cooldown_ticks: 15,
        _reserved: 0,
        launch: ScUnitSpec {
            z: 5.0,
            erp_w: 1.0,
            kind: kind::FIXED_WING_DRONE,
            affiliation: 1,
            options: spawn::PURSUER | spawn::EMITTING,
            outcome_class: 1,
            speed_mps: 80.0,
            max_turn_rate_rad_s: 1.0,
            engagement_radius_m: 10.0,
            ..ScUnitSpec::default()
        },
    }
}

#[test]
fn standing_orders_over_the_c_api() {
    let w = sc_world_create(4);
    let rules = ScRules {
        detection_max_range_m: 20_000.0,
        sensor_base_range_m: 20_000.0,
        sensor_reference_power_w: 1.0,
        exposure_threshold_ticks: 1,
        exposure_cap_ticks: 1,
        exposure_decay_per_tick: 1,
        ..ScRules::default()
    };
    // SAFETY: `w` (and the restored `r`) are live handles used only on this thread and
    // destroyed once; every pointer passed is to a live local.
    unsafe {
        assert!(sc_world_set_rules(w, &rules, std::ptr::null(), 0));
        assert!(sc_world_add_sensor(w, 1, std::ptr::null()));
        let node = Position {
            x: 50_000.0,
            y: 0.0,
            z: 0.0,
        };
        (*w).sim.set_command_node_position(node);
        let mut radar = unit_at(2_000.0, 0.0, 2, kind::GROUND_RADAR);
        radar.options = spawn::EMITTING;
        let target = sc_world_spawn(w, &radar);

        // Rejected without changing anything: no pursuit, a sensor launch, a hostile
        // launch, reserved bits, the stock sentinel, a bad pursuit, a bad zone.
        let mut bad = Vec::new();
        let cases: [fn(&mut ScStandingOrder); 8] = [
            |o| o.launch.options = spawn::EMITTING,
            |o| o.launch.options |= spawn::SENSOR,
            |o| o.launch.affiliation = 2,
            |o| o._reserved = 1,
            |o| o.launch._reserved = 1,
            |o| o.stock = u32::MAX,
            |o| o.launch.speed_mps = 0.0,
            |o| o.radius_m = f64::NAN,
        ];
        for f in cases {
            let mut o = sop(2_000.0, 0.0);
            f(&mut o);
            bad.push(sc_world_add_standing_order(w, &o));
        }
        assert_eq!(bad, [0; 8]);
        assert_eq!((*w).sim.standing_orders().count(), 0);

        let id = sc_world_add_standing_order(w, &sop(2_000.0, 0.0));
        assert_eq!(id, 1);
        // The zone was fixed in the world when the order was added; the launch point
        // follows the node.
        let (_, o) = (*w).sim.standing_orders().next().unwrap();
        assert_eq!((o.center_x, o.center_y), (52_000.0, 0.0));
        (*w).sim.set_command_node_position(Position {
            x: 51_000.0,
            ..node
        });
        sc_world_step(w, 1);
        let l = launches_of(w);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].tick, l[0].order_id, l[0].target_id), (1, id, target));
        let p = (*w).sim.position(StableId(l[0].pursuer_id)).unwrap();
        assert_eq!((p.x, p.y, p.z), (51_000.0, 0.0, 5.0));
        assert_eq!(sc_world_standing_order_stock(w, id), 2);
        assert_eq!(launches_of(w), l, "calling again returns the same launches");

        // Saved with the world: the restored one carries on identically.
        let b = sc_world_save(w);
        let r = sc_world_restore(b.data, b.len);
        assert!(!r.is_null());
        assert_eq!(sc_world_standing_order_stock(r, id), 2);
        assert!(launches_of(r).is_empty(), "empty after a restore");
        for _ in 0..300 {
            sc_world_step(w, 1);
            sc_world_step(r, 1);
            assert_eq!(launches_of(w), launches_of(r));
            assert_eq!(engagements_of(w), engagements_of(r));
        }
        assert!(sc_world_cancel_standing_order(w, id));
        assert!(!sc_world_cancel_standing_order(w, id));
        assert_eq!(sc_world_standing_order_stock(w, id), u32::MAX);
        sc_world_destroy(r);
        sc_world_destroy(w);
    }
}

fn standing_orders_of(w: *mut ScWorld) -> Vec<ScStandingOrderView> {
    // SAFETY: `w` is a live handle on this thread; the orders are copied out before any
    // other call on it, and only read when there are some (NULL otherwise).
    unsafe {
        let s = sc_world_standing_orders(w);
        if s.count == 0 {
            assert!(s.orders.is_null());
            return Vec::new();
        }
        std::slice::from_raw_parts(s.orders, s.count as usize).to_vec()
    }
}

#[test]
fn standing_orders_are_listed_relative_to_the_command_node() {
    let w = sc_world_create(4);
    assert!(standing_orders_of(w).is_empty(), "none in a new world");
    let rules = ScRules {
        detection_max_range_m: 20_000.0,
        sensor_base_range_m: 20_000.0,
        sensor_reference_power_w: 1.0,
        exposure_threshold_ticks: 1,
        exposure_cap_ticks: 1,
        exposure_decay_per_tick: 1,
        ..ScRules::default()
    };
    // SAFETY: `w` (and the restored `r`) are live handles used only on this thread and
    // destroyed once; every pointer passed is to a live local.
    unsafe {
        assert!(sc_world_set_rules(w, &rules, std::ptr::null(), 0));
        assert!(sc_world_add_sensor(w, 1, std::ptr::null()));
        let node = Position {
            x: 50_000.0,
            y: 0.0,
            z: 0.0,
        };
        (*w).sim.set_command_node_position(node);
        let mut radar = unit_at(2_000.0, 0.0, 2, kind::GROUND_RADAR);
        radar.options = spawn::EMITTING;
        let target = sc_world_spawn(w, &radar);

        let id = sc_world_add_standing_order(w, &sop(2_000.0, 0.0));
        let mut idle = sop(-500.0, 250.0);
        (idle.stock, idle.kinds, idle.cooldown_ticks) = (0, 1 << kind::JAMMER, 40);
        let empty = sc_world_add_standing_order(w, &idle);
        assert_eq!((id, empty), (1, 2));
        let view = |id, x, y, radius_m, kinds, remaining, stock, ready_tick, cooldown_ticks| {
            ScStandingOrderView {
                id,
                x,
                y,
                radius_m,
                kinds,
                remaining,
                stock,
                ready_tick,
                cooldown_ticks,
                _reserved: 0,
            }
        };
        let idle_at = |x, y| view(empty, x, y, 300.0, 1 << kind::JAMMER, 0, 0, 0, 40);
        assert_eq!(
            standing_orders_of(w),
            [
                view(id, 2_000.0, 0.0, 300.0, 0, 3, 3, 0, 15),
                idle_at(-500.0, 250.0)
            ],
            "in id order, the out-of-stock one included"
        );

        // The zones stay fixed in the world, so their centres move in the frame as the
        // node moves, exactly as the frame's units do.
        (*w).sim.set_command_node_position(Position {
            x: 51_000.0,
            y: -100.0,
            ..node
        });
        sc_world_step(w, 1);
        let l = launches_of(w);
        assert_eq!((l[0].order_id, l[0].target_id), (id, target));
        let listed = standing_orders_of(w);
        assert_eq!(
            listed,
            [
                view(id, 1_000.0, 100.0, 300.0, 0, 2, 3, 16, 15),
                idle_at(-1_500.0, 350.0)
            ]
        );
        let f = sc_world_frame(w);
        let frame = std::slice::from_raw_parts(f.units, f.count as usize);
        let radar_view = frame.iter().find(|u| u.id == target).unwrap();
        assert_eq!((radar_view.x, radar_view.y), (listed[0].x, listed[0].y));
        assert_eq!(
            standing_orders_of(w),
            listed,
            "calling again lists the same"
        );

        // Saved with the world: the restored one lists the same orders.
        let b = sc_world_save(w);
        let r = sc_world_restore(b.data, b.len);
        assert!(!r.is_null());
        assert_eq!(standing_orders_of(r), listed);

        assert!(sc_world_cancel_standing_order(w, id));
        assert_eq!(standing_orders_of(w), [idle_at(-1_500.0, 350.0)]);
        assert!(sc_world_cancel_standing_order(w, empty));
        assert!(standing_orders_of(w).is_empty(), "NULL once none are left");
        assert_eq!(standing_orders_of(r).len(), 2, "each world its own");
        sc_world_destroy(r);
        sc_world_destroy(w);
    }
}

#[cfg(panic = "unwind")]
#[test]
fn guard_catches_panics() {
    assert_eq!(guard(5, || -> i32 { panic!("boom") }), 5);
    let f = guard(empty_frame(), || -> ScFrame { panic!("boom") });
    assert!(f.units.is_null());
    assert_eq!(guard(1, || 2), 2);
}

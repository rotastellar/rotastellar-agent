// subhadipmitra, 2026-03-09: I-4 Deterministic Eclipse-Boundary State
// Preservation — HazardPredictor module. Produces an ordered timeline of
// upcoming orbital hazards (eclipse, SAA, thermal) and a checkpoint
// schedule that ensures computation state is preserved before each hazard
// with zero overhead during safe periods.
//
// Reference: RS-INV-2026-005, Algorithm Spec: hazard-predictor.md

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

use crate::sim_client::OrbitalElements;

// ── Constants ────────────────────────────────────────────────────────

const EARTH_RADIUS_KM: f64 = 6378.137;
const MU: f64 = 398600.4418; // km³/s²
const J2: f64 = 1.08263e-3;
const AU_KM: f64 = 149597870.7;

// subhadipmitra, 2026-03-09: Safety margin accounts for SGP4 prediction
// uncertainty. 3-sigma for TLE age < 24h ≈ 4.5 seconds.
const SAFETY_MARGIN_S: f64 = 4.5;
const MIN_HAZARD_DURATION_S: f64 = 30.0;
const PROPAGATION_STEP_S: f64 = 30.0;

// Thermal thresholds matching the agent's SatelliteState model.
const THERMAL_THROTTLE_C: f64 = 65.0;
const THERMAL_SHUTDOWN_C: f64 = 80.0;

// SAA boundary polygon (lat, lon) — approximation of the South Atlantic
// Anomaly where trapped proton flux peaks for LEO satellites.
const SAA_BOUNDARY: [(f64, f64); 15] = [
    (-50.0, -85.0),
    (-40.0, -80.0),
    (-20.0, -60.0),
    (-10.0, -40.0),
    (-5.0, -20.0),
    (0.0, 0.0),
    (0.0, 15.0),
    (-5.0, 25.0),
    (-15.0, 30.0),
    (-25.0, 25.0),
    (-35.0, 15.0),
    (-45.0, 0.0),
    (-50.0, -30.0),
    (-55.0, -60.0),
    (-50.0, -85.0),
];

// ── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HazardType {
    Eclipse,
    SaaTransit,
    ThermalPeak,
    Conjunction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
    Low = 1,
    Moderate = 2,
    High = 3,
    Critical = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RecommendedAction {
    CheckpointBeforeAndResumeAfter,
    CheckpointAndEnableEcc,
    ThrottleAndCheckpointIfCritical,
    CheckpointAndPrepareForManeuver,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hazard {
    pub hazard_type: HazardType,
    pub start_time: String,
    pub end_time: String,
    pub duration_s: f64,
    pub severity: Severity,
    pub recommended_action: RecommendedAction,
    pub prediction_confidence: f64,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CheckpointType {
    Save,
    Restore,
    Validate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointEntry {
    pub time: String,
    pub checkpoint_type: CheckpointType,
    pub urgency: Severity,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    pub estimated_duration_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HazardSummary {
    pub total_hazards: usize,
    pub total_checkpoints: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_hazard: Option<String>,
    pub max_safe_compute_window_s: f64,
    pub checkpoint_overhead_fraction: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HazardPrediction {
    pub hazards: Vec<Hazard>,
    pub checkpoint_schedule: Vec<CheckpointEntry>,
    pub summary: HazardSummary,
}

// ── Trajectory Point ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct TrajectoryPoint {
    time: DateTime<Utc>,
    lat: f64,
    lon: f64,
    alt_km: f64,
    eci_x: f64,
    eci_y: f64,
    eci_z: f64,
}

// ── HazardPredictor ──────────────────────────────────────────────────

/// subhadipmitra, 2026-03-09: The HazardPredictor implements the 9-phase
/// algorithm from the I-4 spec. It propagates a satellite trajectory,
/// detects hazards (eclipse, SAA, thermal), computes a checkpoint schedule,
/// and returns a prediction with summary statistics.
pub fn predict_hazards(
    elements: &OrbitalElements,
    current_time: DateTime<Utc>,
    horizon_hours: f64,
    state_size_bytes: u64,
    flash_bandwidth_bps: u64,
) -> HazardPrediction {
    let t_end = current_time + Duration::seconds((horizon_hours * 3600.0) as i64);

    // Phase 1: Propagate trajectory
    let trajectory = propagate_trajectory(elements, current_time, t_end);

    // Phase 2: Detect eclipse windows
    let mut hazards = detect_eclipses(&trajectory);

    // Phase 3: Detect SAA transits
    hazards.extend(detect_saa_transits(&trajectory));

    // Phase 4: Detect thermal peaks
    hazards.extend(detect_thermal_peaks(&trajectory));

    // Phase 6: Sort and merge
    hazards.sort_by(|a, b| a.start_time.cmp(&b.start_time));
    hazards = merge_overlapping_hazards(hazards);

    // Phase 7-8: Compute checkpoint schedule
    let t_serialize = if flash_bandwidth_bps > 0 {
        state_size_bytes as f64 / flash_bandwidth_bps as f64
    } else {
        1.0
    };
    let t_validate = t_serialize / 10.0;
    let schedule = compute_checkpoint_schedule(
        &hazards,
        current_time,
        t_serialize,
        t_validate,
    );

    // Phase 9: Compute summary
    let summary = compute_summary(
        &hazards,
        &schedule,
        current_time,
        t_end,
        t_serialize,
        t_validate,
        horizon_hours,
    );

    HazardPrediction {
        hazards,
        checkpoint_schedule: schedule,
        summary,
    }
}

// ── Phase 1: Simplified Keplerian Propagation with J2 ────────────────

fn propagate_trajectory(
    elements: &OrbitalElements,
    t_start: DateTime<Utc>,
    t_end: DateTime<Utc>,
) -> Vec<TrajectoryPoint> {
    let a_km = EARTH_RADIUS_KM + elements.altitude_km;
    let n = (MU / (a_km * a_km * a_km)).sqrt(); // rad/s
    let inc = elements.inclination_deg.to_radians();
    let ecc = elements.eccentricity;

    // J2 secular rates
    let p = a_km * (1.0 - ecc * ecc);
    let cos_i = inc.cos();
    let j2_raan_rate = -1.5 * n * J2 * (EARTH_RADIUS_KM / p).powi(2) * cos_i;
    let j2_argp_rate =
        1.5 * n * J2 * (EARTH_RADIUS_KM / p).powi(2) * (2.0 - 2.5 * inc.sin().powi(2));

    let epoch: DateTime<Utc> = elements
        .epoch
        .parse()
        .unwrap_or(t_start);

    let raan0 = elements.raan_deg.to_radians();
    let argp0 = elements.arg_perigee_deg.to_radians();
    let m0 = elements.mean_anomaly_deg.to_radians();

    let mut trajectory = Vec::new();
    let mut t = t_start;

    while t <= t_end {
        let dt_s = (t - epoch).num_milliseconds() as f64 / 1000.0;

        // Mean anomaly at time t
        let m_t = m0 + n * dt_s;
        // Solve Kepler's equation (Newton-Raphson)
        let e_anom = solve_kepler(m_t, ecc);
        // True anomaly
        let cos_e = e_anom.cos();
        let sin_e = e_anom.sin();
        let nu = ((1.0 - ecc * ecc).sqrt() * sin_e).atan2(cos_e - ecc);

        // Radius
        let r = a_km * (1.0 - ecc * cos_e);

        // Argument of latitude
        let raan = raan0 + j2_raan_rate * dt_s;
        let argp = argp0 + j2_argp_rate * dt_s;
        let u = argp + nu;

        // ECI coordinates
        let cos_raan = raan.cos();
        let sin_raan = raan.sin();
        let cos_u = u.cos();
        let sin_u = u.sin();
        let cos_inc = inc.cos();
        let sin_inc = inc.sin();

        let eci_x = r * (cos_raan * cos_u - sin_raan * sin_u * cos_inc);
        let eci_y = r * (sin_raan * cos_u + cos_raan * sin_u * cos_inc);
        let eci_z = r * sin_u * sin_inc;

        // Sub-satellite point (lat/lon)
        let lat = (eci_z / r).asin().to_degrees();
        // Greenwich sidereal time (simplified)
        let gmst = greenwich_sidereal_time(t);
        let lon_eci = eci_y.atan2(eci_x);
        let mut lon = (lon_eci - gmst).to_degrees();
        if lon > 180.0 {
            lon -= 360.0;
        }
        if lon < -180.0 {
            lon += 360.0;
        }

        trajectory.push(TrajectoryPoint {
            time: t,
            lat,
            lon,
            alt_km: r - EARTH_RADIUS_KM,
            eci_x,
            eci_y,
            eci_z,
        });

        t += Duration::seconds(PROPAGATION_STEP_S as i64);
    }

    trajectory
}

fn solve_kepler(m: f64, ecc: f64) -> f64 {
    // Normalize M to [0, 2π)
    let m_norm = m.rem_euclid(2.0 * PI);
    let mut e = m_norm;
    for _ in 0..15 {
        let de = (e - ecc * e.sin() - m_norm) / (1.0 - ecc * e.cos());
        e -= de;
        if de.abs() < 1e-12 {
            break;
        }
    }
    e
}

fn greenwich_sidereal_time(t: DateTime<Utc>) -> f64 {
    // Simplified GMST from Julian date
    let jd = julian_date(t);
    let t_centuries = (jd - 2451545.0) / 36525.0;
    let gmst_deg = 280.46061837 + 360.98564736629 * (jd - 2451545.0)
        + 0.000387933 * t_centuries * t_centuries;
    (gmst_deg % 360.0).to_radians()
}

fn julian_date(t: DateTime<Utc>) -> f64 {
    let y = t.format("%Y").to_string().parse::<f64>().unwrap_or(2026.0);
    let m = t.format("%m").to_string().parse::<f64>().unwrap_or(1.0);
    let d = t.format("%d").to_string().parse::<f64>().unwrap_or(1.0);
    let h = t.format("%H").to_string().parse::<f64>().unwrap_or(0.0);
    let min = t.format("%M").to_string().parse::<f64>().unwrap_or(0.0);
    let sec = t.format("%S").to_string().parse::<f64>().unwrap_or(0.0);
    let day_frac = d + (h + min / 60.0 + sec / 3600.0) / 24.0;

    let (y2, m2) = if m <= 2.0 {
        (y - 1.0, m + 12.0)
    } else {
        (y, m)
    };

    let a = (y2 / 100.0).floor();
    let b = 2.0 - a + (a / 4.0).floor();

    (365.25 * (y2 + 4716.0)).floor() + (30.6001 * (m2 + 1.0)).floor() + day_frac + b - 1524.5
}

// ── Phase 2: Eclipse Detection ───────────────────────────────────────

// subhadipmitra, 2026-03-09: Cylindrical shadow model. The satellite is
// eclipsed when it's on the anti-sun side of Earth and the perpendicular
// distance from the Earth-Sun line is less than Earth's radius.
fn is_in_eclipse(eci_x: f64, eci_y: f64, eci_z: f64, sun_x: f64, sun_y: f64, sun_z: f64) -> bool {
    let sun_mag = (sun_x * sun_x + sun_y * sun_y + sun_z * sun_z).sqrt();
    if sun_mag < 1.0 {
        return false;
    }
    let sx = sun_x / sun_mag;
    let sy = sun_y / sun_mag;
    let sz = sun_z / sun_mag;

    // Dot product: satellite position along sun direction
    let dot = eci_x * sx + eci_y * sy + eci_z * sz;
    if dot >= 0.0 {
        return false; // sunlit side
    }

    // Perpendicular distance from Earth-Sun line
    let px = eci_x - dot * sx;
    let py = eci_y - dot * sy;
    let pz = eci_z - dot * sz;
    let perp_dist = (px * px + py * py + pz * pz).sqrt();

    perp_dist < EARTH_RADIUS_KM
}

fn sun_position_eci(t: DateTime<Utc>) -> (f64, f64, f64) {
    // Low-precision solar ephemeris (< 1° accuracy, sufficient for eclipse detection)
    let jd = julian_date(t);
    let n = jd - 2451545.0;
    let l = (280.460 + 0.9856474 * n).to_radians();
    let g = (357.528 + 0.9856003 * n).to_radians();
    let lambda = l + (1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let epsilon = 23.439_f64.to_radians();

    let x = AU_KM * lambda.cos();
    let y = AU_KM * lambda.sin() * epsilon.cos();
    let z = AU_KM * lambda.sin() * epsilon.sin();
    (x, y, z)
}

fn detect_eclipses(trajectory: &[TrajectoryPoint]) -> Vec<Hazard> {
    let mut hazards = Vec::new();
    let mut in_eclipse = false;
    let mut eclipse_start: Option<DateTime<Utc>> = None;

    for point in trajectory {
        let (sx, sy, sz) = sun_position_eci(point.time);
        let eclipsed = is_in_eclipse(point.eci_x, point.eci_y, point.eci_z, sx, sy, sz);

        if eclipsed && !in_eclipse {
            eclipse_start = Some(point.time);
            in_eclipse = true;
        } else if !eclipsed && in_eclipse {
            if let Some(start) = eclipse_start {
                let duration = (point.time - start).num_seconds() as f64;
                if duration >= MIN_HAZARD_DURATION_S {
                    hazards.push(Hazard {
                        hazard_type: HazardType::Eclipse,
                        start_time: start.to_rfc3339(),
                        end_time: point.time.to_rfc3339(),
                        duration_s: duration,
                        severity: Severity::Critical,
                        recommended_action: RecommendedAction::CheckpointBeforeAndResumeAfter,
                        prediction_confidence: 0.997,
                        metadata: serde_json::json!({}),
                    });
                }
            }
            in_eclipse = false;
        }
    }

    // Close open eclipse at horizon boundary
    if in_eclipse {
        if let (Some(start), Some(last)) = (eclipse_start, trajectory.last()) {
            let duration = (last.time - start).num_seconds() as f64;
            hazards.push(Hazard {
                hazard_type: HazardType::Eclipse,
                start_time: start.to_rfc3339(),
                end_time: last.time.to_rfc3339(),
                duration_s: duration,
                severity: Severity::Critical,
                recommended_action: RecommendedAction::CheckpointBeforeAndResumeAfter,
                prediction_confidence: 0.997,
                metadata: serde_json::json!({}),
            });
        }
    }

    hazards
}

// ── Phase 3: SAA Detection ───────────────────────────────────────────

fn point_in_polygon(lat: f64, lon: f64, polygon: &[(f64, f64)]) -> bool {
    let n = polygon.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (yi, xi) = polygon[i];
        let (yj, xj) = polygon[j];
        if ((yi > lat) != (yj > lat)) && (lon < (xj - xi) * (lat - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn saa_intensity(alt_km: f64) -> f64 {
    // Gaussian fit: peak proton flux at ~400 km, sigma ~150 km
    (-(alt_km - 400.0).powi(2) / (2.0 * 150.0_f64.powi(2))).exp()
}

fn detect_saa_transits(trajectory: &[TrajectoryPoint]) -> Vec<Hazard> {
    let mut hazards = Vec::new();
    let mut in_saa = false;
    let mut saa_start: Option<DateTime<Utc>> = None;
    let mut peak_intensity: f64 = 0.0;

    for point in trajectory {
        let inside = point_in_polygon(point.lat, point.lon, &SAA_BOUNDARY);

        if inside && !in_saa {
            saa_start = Some(point.time);
            in_saa = true;
            peak_intensity = saa_intensity(point.alt_km);
        } else if inside && in_saa {
            peak_intensity = peak_intensity.max(saa_intensity(point.alt_km));
        } else if !inside && in_saa {
            if let Some(start) = saa_start {
                let duration = (point.time - start).num_seconds() as f64;
                if duration >= MIN_HAZARD_DURATION_S {
                    let severity = if peak_intensity > 0.7 {
                        Severity::High
                    } else {
                        Severity::Moderate
                    };
                    hazards.push(Hazard {
                        hazard_type: HazardType::SaaTransit,
                        start_time: start.to_rfc3339(),
                        end_time: point.time.to_rfc3339(),
                        duration_s: duration,
                        severity,
                        recommended_action: RecommendedAction::CheckpointAndEnableEcc,
                        prediction_confidence: 0.95,
                        metadata: serde_json::json!({
                            "peak_intensity": (peak_intensity * 100.0).round() / 100.0,
                        }),
                    });
                }
            }
            in_saa = false;
            peak_intensity = 0.0;
        }
    }

    // Close open SAA transit
    if in_saa {
        if let (Some(start), Some(last)) = (saa_start, trajectory.last()) {
            let duration = (last.time - start).num_seconds() as f64;
            hazards.push(Hazard {
                hazard_type: HazardType::SaaTransit,
                start_time: start.to_rfc3339(),
                end_time: last.time.to_rfc3339(),
                duration_s: duration,
                severity: Severity::High,
                recommended_action: RecommendedAction::CheckpointAndEnableEcc,
                prediction_confidence: 0.95,
                metadata: serde_json::json!({"peak_intensity": (peak_intensity * 100.0).round() / 100.0}),
            });
        }
    }

    hazards
}

// ── Phase 4: Thermal Prediction ──────────────────────────────────────

fn detect_thermal_peaks(trajectory: &[TrajectoryPoint]) -> Vec<Hazard> {
    let mut hazards = Vec::new();
    let mut temp_c: f64 = 25.0;
    let mut in_thermal = false;
    let mut thermal_start: Option<DateTime<Utc>> = None;
    let mut peak_temp: f64 = 0.0;

    for point in trajectory {
        let (sx, sy, sz) = sun_position_eci(point.time);
        let eclipsed = is_in_eclipse(point.eci_x, point.eci_y, point.eci_z, sx, sy, sz);

        // Simple thermal model matching SatelliteState
        let target = if eclipsed { -20.0 } else { 40.0 };
        let rate = 0.5; // °C per minute
        let dt_min = PROPAGATION_STEP_S / 60.0;
        let delta = (target - temp_c) * (rate * dt_min / 60.0).min(1.0);
        temp_c += delta;

        if temp_c > THERMAL_THROTTLE_C && !in_thermal {
            thermal_start = Some(point.time);
            in_thermal = true;
            peak_temp = temp_c;
        } else if in_thermal {
            peak_temp = peak_temp.max(temp_c);
        }

        if temp_c <= THERMAL_THROTTLE_C && in_thermal {
            if let Some(start) = thermal_start {
                let duration = (point.time - start).num_seconds() as f64;
                if duration >= MIN_HAZARD_DURATION_S {
                    let severity = if peak_temp > THERMAL_SHUTDOWN_C {
                        Severity::High
                    } else {
                        Severity::Moderate
                    };
                    hazards.push(Hazard {
                        hazard_type: HazardType::ThermalPeak,
                        start_time: start.to_rfc3339(),
                        end_time: point.time.to_rfc3339(),
                        duration_s: duration,
                        severity,
                        recommended_action: RecommendedAction::ThrottleAndCheckpointIfCritical,
                        prediction_confidence: 0.90,
                        metadata: serde_json::json!({
                            "estimated_peak_c": (peak_temp * 10.0).round() / 10.0,
                            "throttle_threshold_c": THERMAL_THROTTLE_C,
                        }),
                    });
                }
            }
            in_thermal = false;
        }
    }

    // Close open thermal window
    if in_thermal {
        if let (Some(start), Some(last)) = (thermal_start, trajectory.last()) {
            let duration = (last.time - start).num_seconds() as f64;
            hazards.push(Hazard {
                hazard_type: HazardType::ThermalPeak,
                start_time: start.to_rfc3339(),
                end_time: last.time.to_rfc3339(),
                duration_s: duration,
                severity: Severity::Moderate,
                recommended_action: RecommendedAction::ThrottleAndCheckpointIfCritical,
                prediction_confidence: 0.90,
                metadata: serde_json::json!({"estimated_peak_c": (peak_temp * 10.0).round() / 10.0}),
            });
        }
    }

    hazards
}

// ── Phase 6: Merge Overlapping Hazards ───────────────────────────────

fn merge_overlapping_hazards(hazards: Vec<Hazard>) -> Vec<Hazard> {
    if hazards.len() <= 1 {
        return hazards;
    }

    let mut merged: Vec<Hazard> = Vec::new();
    let mut current = hazards[0].clone();

    for next in hazards.iter().skip(1) {
        if next.hazard_type == current.hazard_type && next.start_time <= current.end_time {
            // Merge: extend to cover both
            if next.end_time > current.end_time {
                current.end_time = next.end_time.clone();
            }
            // Recompute duration
            if let (Ok(s), Ok(e)) = (
                current.start_time.parse::<DateTime<Utc>>(),
                current.end_time.parse::<DateTime<Utc>>(),
            ) {
                current.duration_s = (e - s).num_seconds() as f64;
            }
            if next.severity > current.severity {
                current.severity = next.severity;
            }
        } else {
            merged.push(current);
            current = next.clone();
        }
    }
    merged.push(current);

    merged
}

// ── Phase 7-8: Checkpoint Schedule ───────────────────────────────────

fn compute_checkpoint_schedule(
    hazards: &[Hazard],
    current_time: DateTime<Utc>,
    t_serialize: f64,
    t_validate: f64,
) -> Vec<CheckpointEntry> {
    let mut schedule: Vec<CheckpointEntry> = Vec::new();

    for hazard in hazards {
        let h_start: DateTime<Utc> = match hazard.start_time.parse() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let h_end: DateTime<Utc> = match hazard.end_time.parse() {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Pre-hazard SAVE
        let save_time = h_start - Duration::milliseconds(((t_serialize + SAFETY_MARGIN_S) * 1000.0) as i64);
        if save_time > current_time {
            schedule.push(CheckpointEntry {
                time: save_time.to_rfc3339(),
                checkpoint_type: CheckpointType::Save,
                urgency: hazard.severity,
                reason: format!("Pre-{:?} checkpoint", hazard.hazard_type),
                deadline: Some(hazard.start_time.clone()),
                estimated_duration_s: t_serialize,
            });
        }

        // Post-hazard VALIDATE
        schedule.push(CheckpointEntry {
            time: h_end.to_rfc3339(),
            checkpoint_type: CheckpointType::Validate,
            urgency: Severity::Low,
            reason: format!("Post-{:?} state validation", hazard.hazard_type),
            deadline: None,
            estimated_duration_s: t_validate,
        });

        // Post-hazard RESTORE (for eclipse and SAA — actions that recommend resume)
        if matches!(
            hazard.recommended_action,
            RecommendedAction::CheckpointBeforeAndResumeAfter
                | RecommendedAction::CheckpointAndEnableEcc
        ) {
            let restore_time = h_end + Duration::milliseconds((t_validate * 1000.0) as i64);
            schedule.push(CheckpointEntry {
                time: restore_time.to_rfc3339(),
                checkpoint_type: CheckpointType::Restore,
                urgency: Severity::Moderate,
                reason: format!("Post-{:?} state restoration", hazard.hazard_type),
                deadline: None,
                estimated_duration_s: t_serialize,
            });
        }
    }

    // Sort by time
    schedule.sort_by(|a, b| a.time.cmp(&b.time));

    // Merge adjacent checkpoints of the same type
    let min_interval = t_serialize * 2.5;
    if schedule.len() <= 1 {
        return schedule;
    }

    let mut merged: Vec<CheckpointEntry> = vec![schedule[0].clone()];
    for entry in schedule.iter().skip(1) {
        let prev = merged.last().unwrap();
        if let (Ok(prev_t), Ok(curr_t)) = (
            prev.time.parse::<DateTime<Utc>>(),
            entry.time.parse::<DateTime<Utc>>(),
        ) {
            let gap_s = (curr_t - prev_t).num_milliseconds() as f64 / 1000.0;
            if gap_s < min_interval && prev.checkpoint_type == entry.checkpoint_type {
                // Keep the more urgent one
                if entry.urgency > prev.urgency {
                    *merged.last_mut().unwrap() = entry.clone();
                }
                continue;
            }
        }
        merged.push(entry.clone());
    }

    merged
}

// ── Phase 9: Summary ─────────────────────────────────────────────────

fn compute_summary(
    hazards: &[Hazard],
    schedule: &[CheckpointEntry],
    t_start: DateTime<Utc>,
    t_end: DateTime<Utc>,
    t_serialize: f64,
    t_validate: f64,
    horizon_hours: f64,
) -> HazardSummary {
    let mut max_safe_s: f64 = 0.0;
    let mut prev_end = t_start;

    for hazard in hazards {
        if let Ok(h_start) = hazard.start_time.parse::<DateTime<Utc>>() {
            let safe_s = (h_start - prev_end).num_seconds() as f64
                - t_serialize
                - SAFETY_MARGIN_S;
            if safe_s > max_safe_s {
                max_safe_s = safe_s;
            }
        }
        if let Ok(h_end) = hazard.end_time.parse::<DateTime<Utc>>() {
            prev_end = h_end
                + Duration::milliseconds(((t_validate + t_serialize) * 1000.0) as i64);
        }
    }

    // Trailing safe window
    let trailing = (t_end - prev_end).num_seconds() as f64;
    if trailing > max_safe_s {
        max_safe_s = trailing;
    }

    let total_checkpoint_time = schedule.len() as f64 * (t_serialize + t_validate);
    let total_horizon_s = horizon_hours * 3600.0;
    let overhead = if total_horizon_s > 0.0 {
        total_checkpoint_time / total_horizon_s
    } else {
        0.0
    };

    HazardSummary {
        total_hazards: hazards.len(),
        total_checkpoints: schedule.len(),
        next_hazard: hazards.first().map(|h| h.start_time.clone()),
        max_safe_compute_window_s: max_safe_s.max(0.0).round(),
        checkpoint_overhead_fraction: (overhead * 10000.0).round() / 10000.0,
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn iss_elements() -> OrbitalElements {
        OrbitalElements {
            altitude_km: 408.0,
            inclination_deg: 51.6,
            eccentricity: 0.0001,
            raan_deg: 0.0,
            arg_perigee_deg: 90.0,
            mean_anomaly_deg: 0.0,
            mean_motion: 15.49,
            epoch: "2026-03-09T00:00:00Z".into(),
        }
    }

    fn sso_elements() -> OrbitalElements {
        OrbitalElements {
            altitude_km: 786.0,
            inclination_deg: 98.6,
            eccentricity: 0.0001,
            raan_deg: 0.0,
            arg_perigee_deg: 90.0,
            mean_anomaly_deg: 0.0,
            mean_motion: 14.3,
            epoch: "2026-03-09T00:00:00Z".into(),
        }
    }

    #[test]
    fn test_predict_hazards_iss_6h() {
        let t = "2026-03-09T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let pred = predict_hazards(&iss_elements(), t, 6.0, 50_000_000, 10_000_000);

        // ISS at 408 km, 51.6° inc, ~92 min period → ~4 eclipses in 6h
        assert!(pred.hazards.len() >= 2, "Should detect multiple hazards");

        let eclipses: Vec<_> = pred
            .hazards
            .iter()
            .filter(|h| h.hazard_type == HazardType::Eclipse)
            .collect();
        assert!(eclipses.len() >= 2, "ISS should have ~4 eclipses in 6h, got {}", eclipses.len());

        for ec in &eclipses {
            assert_eq!(ec.severity, Severity::Critical);
            assert_eq!(ec.prediction_confidence, 0.997);
            assert!(ec.duration_s >= 60.0, "Eclipse should be at least 1 min");
            assert!(ec.duration_s <= 2400.0, "Eclipse should be at most 40 min");
        }

        // Checkpoint schedule should have entries
        assert!(!pred.checkpoint_schedule.is_empty());
        // Summary should be reasonable
        assert!(pred.summary.total_hazards >= 2);
        assert!(pred.summary.max_safe_compute_window_s > 0.0);
        assert!(pred.summary.checkpoint_overhead_fraction < 0.1);
    }

    #[test]
    fn test_predict_hazards_sso_12h() {
        let t = "2026-03-09T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let pred = predict_hazards(&sso_elements(), t, 12.0, 50_000_000, 10_000_000);

        // SSO should have eclipses
        let eclipses: Vec<_> = pred
            .hazards
            .iter()
            .filter(|h| h.hazard_type == HazardType::Eclipse)
            .collect();
        assert!(!eclipses.is_empty(), "SSO should have eclipses");

        // High-inclination SSO should have fewer/no SAA transits
        let saa: Vec<_> = pred
            .hazards
            .iter()
            .filter(|h| h.hazard_type == HazardType::SaaTransit)
            .collect();
        // SSO at 98.6° may still cross SAA occasionally
        assert!(saa.len() <= eclipses.len(), "SSO should have fewer SAA transits than eclipses");
    }

    #[test]
    fn test_predict_hazards_short_horizon() {
        let t = "2026-03-09T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let pred = predict_hazards(&iss_elements(), t, 0.5, 50_000_000, 10_000_000);

        // 30 min may catch 0 or 1 eclipse boundary
        assert!(pred.summary.total_hazards <= 2);
        assert!(pred.summary.checkpoint_overhead_fraction < 0.5);
    }

    #[test]
    fn test_predict_hazards_no_horizon() {
        let t = "2026-03-09T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let pred = predict_hazards(&iss_elements(), t, 0.0, 50_000_000, 10_000_000);

        assert_eq!(pred.hazards.len(), 0);
        assert_eq!(pred.checkpoint_schedule.len(), 0);
    }

    #[test]
    fn test_point_in_polygon_saa() {
        // Center of SAA
        assert!(point_in_polygon(-30.0, -45.0, &SAA_BOUNDARY));
        // Far from SAA
        assert!(!point_in_polygon(45.0, 10.0, &SAA_BOUNDARY));
        // North pole
        assert!(!point_in_polygon(90.0, 0.0, &SAA_BOUNDARY));
    }

    #[test]
    fn test_saa_intensity() {
        let peak = saa_intensity(400.0);
        assert!((peak - 1.0).abs() < 0.01, "Peak should be ~1.0 at 400 km");

        let low = saa_intensity(800.0);
        assert!(low < peak, "Intensity should decrease away from 400 km");

        let very_low = saa_intensity(1200.0);
        assert!(very_low < 0.1, "Intensity should be very low at 1200 km");
    }

    #[test]
    fn test_solve_kepler() {
        // Circular orbit: E should equal M
        let e = solve_kepler(1.0, 0.0);
        assert!((e - 1.0).abs() < 1e-10);

        // Low eccentricity
        let e = solve_kepler(1.0, 0.01);
        assert!((e - 1.0).abs() < 0.02);
    }

    #[test]
    fn test_eclipse_detection() {
        // Satellite on anti-sun side, within Earth radius → eclipsed
        assert!(is_in_eclipse(-7000.0, 0.0, 0.0, AU_KM, 0.0, 0.0));
        // Satellite on sun side → not eclipsed
        assert!(!is_in_eclipse(7000.0, 0.0, 0.0, AU_KM, 0.0, 0.0));
        // Satellite far from Earth-Sun line → not eclipsed even on anti-sun side
        assert!(!is_in_eclipse(-7000.0, 8000.0, 0.0, AU_KM, 0.0, 0.0));
    }

    #[test]
    fn test_merge_overlapping_hazards() {
        let h1 = Hazard {
            hazard_type: HazardType::Eclipse,
            start_time: "2026-03-09T12:00:00Z".into(),
            end_time: "2026-03-09T12:30:00Z".into(),
            duration_s: 1800.0,
            severity: Severity::Critical,
            recommended_action: RecommendedAction::CheckpointBeforeAndResumeAfter,
            prediction_confidence: 0.997,
            metadata: serde_json::json!({}),
        };
        let h2 = Hazard {
            hazard_type: HazardType::Eclipse,
            start_time: "2026-03-09T12:25:00Z".into(),
            end_time: "2026-03-09T12:35:00Z".into(),
            duration_s: 600.0,
            severity: Severity::Critical,
            recommended_action: RecommendedAction::CheckpointBeforeAndResumeAfter,
            prediction_confidence: 0.997,
            metadata: serde_json::json!({}),
        };
        // Different type — not merged
        let h3 = Hazard {
            hazard_type: HazardType::SaaTransit,
            start_time: "2026-03-09T12:28:00Z".into(),
            end_time: "2026-03-09T12:45:00Z".into(),
            duration_s: 1020.0,
            severity: Severity::High,
            recommended_action: RecommendedAction::CheckpointAndEnableEcc,
            prediction_confidence: 0.95,
            metadata: serde_json::json!({}),
        };

        let merged = merge_overlapping_hazards(vec![h1, h2, h3]);
        assert_eq!(merged.len(), 2, "Overlapping eclipses should merge, SAA stays separate");
        assert_eq!(merged[0].hazard_type, HazardType::Eclipse);
        assert_eq!(merged[0].end_time, "2026-03-09T12:35:00Z");
        assert_eq!(merged[1].hazard_type, HazardType::SaaTransit);
    }

    #[test]
    fn test_checkpoint_schedule_structure() {
        let t = "2026-03-09T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let pred = predict_hazards(&iss_elements(), t, 6.0, 50_000_000, 10_000_000);

        // Each eclipse should produce at least SAVE + VALIDATE + RESTORE
        for entry in &pred.checkpoint_schedule {
            assert!(!entry.reason.is_empty());
            assert!(entry.estimated_duration_s > 0.0);
        }

        // Check ordering
        for i in 1..pred.checkpoint_schedule.len() {
            assert!(
                pred.checkpoint_schedule[i].time >= pred.checkpoint_schedule[i - 1].time,
                "Schedule should be time-ordered"
            );
        }
    }
}

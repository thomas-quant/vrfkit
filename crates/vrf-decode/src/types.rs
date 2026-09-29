//! Model types for Unreal replay values, as plain data; their `Display` impls
//! produce the string written to `value_str`.

use core::fmt;

/// How [`FRepMovement`] serializes its rotation, per axis:
/// - `ByteComponents`: 1 flag bit + 8 data bits (+/-1.4deg precision)
/// - `ShortComponents`: 1 flag bit + 16 data bits (+/-0.005deg)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RotatorQuantization {
    ByteComponents,
    ShortComponents,
}

/// Location quantization for [`FRepMovement`] (Unreal's `EVectorQuantization`):
/// the decimals the sending class rounds to before packing. Not on the wire --
/// the packed header says only "scaled" -- so every `RepMovement` entry states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorQuantization {
    /// Whole units: the packed integer is the coordinate.
    RoundWholeNumber,
    /// One decimal: the packed integer is ten times the coordinate.
    RoundOneDecimal,
    /// Two decimals: the packed integer is a hundred times the coordinate.
    RoundTwoDecimals,
}

impl VectorQuantization {
    /// The divisor that turns a scaled packed integer back into world units.
    pub const fn scale(self) -> u32 {
        match self {
            Self::RoundWholeNumber => 1,
            Self::RoundOneDecimal => 10,
            Self::RoundTwoDecimals => 100,
        }
    }
}

/// A 3D vector; components are `f64` whatever the wire width (f32 is widened).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FVector {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl fmt::Display for FVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({},{},{})", self.x, self.y, self.z)
    }
}

/// Euler rotation (degrees).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FRotator {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

impl fmt::Display for FRotator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rot({},{},{})", self.pitch, self.yaw, self.roll)
    }
}

/// Quaternion rotation (4 x f32).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FQuat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl fmt::Display for FQuat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "quat({},{},{},{})", self.x, self.y, self.z, self.w)
    }
}

/// Transform = rotation (quat) + translation (vec3) + scale (vec3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FTransform {
    pub rotation: FQuat,
    pub translation: FVector,
    pub scale: FVector,
}

impl fmt::Display for FTransform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "xform({};{};{})",
            self.rotation, self.translation, self.scale
        )
    }
}

/// Replicated movement state, in wire order:
///
/// ```text
/// 4 bits: bSimulatedPhysicsSleep, bRepPhysics, bRepServerFrame, bRepServerHandle
/// location: packed quantized vector / VectorQuantization::scale()
/// rotation: RotationShort or RotationByte; linear velocity: whole units
/// if bRepPhysics: angular velocity (whole units)
/// if bRepServerFrame / bRepServerHandle: IntPacked server frame / physics handle
/// ```
///
/// The location divisor and rotator width are per-class choices the wire does
/// not carry; `FieldType::RepMovement` supplies both.
#[derive(Debug, Clone, PartialEq)]
pub struct FRepMovement {
    pub location: FVector,
    pub rotation: FRotator,
    pub linear_velocity: FVector,
    pub angular_velocity: Option<FVector>,
    pub simulated_physics_sleep: bool,
    pub rep_physics: bool,
    /// `None` when `bRepServerFrame` is clear: not sent, not frame 0.
    pub server_frame: Option<u32>,
    /// `None` when `bRepServerHandle` is clear.
    pub server_physics_handle: Option<u32>,
}

/// Writes an [`FVector`] as `{"x":..,"y":..,"z":..}`, not its compact `Display`.
fn write_vector_json(f: &mut fmt::Formatter<'_>, v: &FVector) -> fmt::Result {
    write!(f, "{{\"x\":{},\"y\":{},\"z\":{}}}", v.x, v.y, v.z)
}

/// A JSON object, not the compact form, which has nowhere to put
/// `simulated_physics_sleep` or `server_physics_handle`. Member names and
/// order follow the reference bundle exactly
/// (docs/archive/PROJECT_STATUS.md 13-B: a 14,377-row regression). Finiteness
/// is enforced by
/// `DecodeError::NonFiniteComponent`, not by construction: the
/// componentBitCount == 0 raw-float fallback can carry NaN (docs/OVERLAY_RESOLUTION.md
/// "FRepMovement finiteness is enforced"). Any other constructor owes that check.
impl fmt::Display for FRepMovement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{\"linear_velocity\":")?;
        write_vector_json(f, &self.linear_velocity)?;
        f.write_str(",\"angular_velocity\":")?;
        match self.angular_velocity {
            Some(ref av) => write_vector_json(f, av)?,
            None => f.write_str("null")?,
        }
        f.write_str(",\"location\":")?;
        write_vector_json(f, &self.location)?;
        write!(
            f,
            ",\"rotation\":{{\"pitch\":{},\"yaw\":{},\"roll\":{}}}",
            self.rotation.pitch, self.rotation.yaw, self.rotation.roll
        )?;
        write!(
            f,
            ",\"simulated_physics_sleep\":{},\"rep_physics\":{}",
            self.simulated_physics_sleep, self.rep_physics
        )?;
        f.write_str(",\"server_frame\":")?;
        match self.server_frame {
            Some(v) => write!(f, "{v}")?,
            None => f.write_str("null")?,
        }
        f.write_str(",\"server_physics_handle\":")?;
        match self.server_physics_handle {
            Some(v) => write!(f, "{v}")?,
            None => f.write_str("null")?,
        }
        f.write_str("}")
    }
}

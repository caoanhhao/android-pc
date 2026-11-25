//! The media controller (`linux/media.h`) and V4L2 sub-devices (`linux/v4l2-subdev.h`),
//! for cameras whose sensor, receiver and DMA engine are separate entities (Qualcomm
//! CAMSS): read the graph, enable links, set pad formats and the sensor's controls.
//!
//! Only `libc` and `std`, so the raw capture tool can build outside Soong too.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::ptr;

/// Runs one ioctl, retrying on `EINTR`.
///
/// # Safety
///
/// `request` must be an ioctl whose argument is a `T`.
unsafe fn ioctl<T>(fd: BorrowedFd<'_>, request: u32, arg: &mut T) -> io::Result<()> {
    loop {
        // SAFETY: the caller guarantees `arg` has the type `request` expects.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, ptr::from_mut(arg)) };
        if ret != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

const fn ioc(dir: u32, kind: u8, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((kind as u32) << 8) | nr
}
const READ: u32 = 2;
const WRITE: u32 = 1;

#[repr(C)]
#[derive(Default)]
struct Topology {
    topology_version: u64,
    num_entities: u32,
    reserved1: u32,
    ptr_entities: u64,
    num_interfaces: u32,
    reserved2: u32,
    ptr_interfaces: u64,
    num_pads: u32,
    reserved3: u32,
    ptr_pads: u64,
    num_links: u32,
    reserved4: u32,
    ptr_links: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawEntity {
    id: u32,
    name: [u8; 64],
    function: u32,
    flags: u32,
    reserved: [u32; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInterface {
    id: u32,
    intf_type: u32,
    flags: u32,
    reserved: [u32; 9],
    /// `devnode { major, minor }` or 16 raw words.
    raw: [u32; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawPad {
    id: u32,
    entity_id: u32,
    flags: u32,
    index: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawLink {
    id: u32,
    source_id: u32,
    sink_id: u32,
    flags: u32,
    reserved: [u32; 6],
}

#[repr(C)]
#[derive(Default)]
struct PadDesc {
    entity: u32,
    index: u16,
    flags: u32,
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct LinkDesc {
    source: PadDesc,
    sink: PadDesc,
    flags: u32,
    reserved: [u32; 2],
}

const MEDIA_IOC_SETUP_LINK: u32 = ioc(READ | WRITE, b'|', 0x03, size_of::<LinkDesc>());
const MEDIA_IOC_G_TOPOLOGY: u32 = ioc(READ | WRITE, b'|', 0x04, size_of::<Topology>());

const PAD_FL_SINK: u32 = 1;
const LNK_FL_ENABLED: u32 = 1;
const LNK_FL_IMMUTABLE: u32 = 2;
const LNK_FL_LINK_TYPE: u32 = 0xf << 28;
const LNK_FL_DATA_LINK: u32 = 0;
const LNK_FL_INTERFACE_LINK: u32 = 1 << 28;
const LNK_FL_ANCILLARY_LINK: u32 = 2 << 28;

/// `MEDIA_ENT_F_CAM_SENSOR`.
pub const FUNCTION_CAMERA_SENSOR: u32 = 0x0002_0001;
/// `MEDIA_ENT_F_LENS`.
pub const FUNCTION_LENS: u32 = 0x0002_0003;

/// One entity of the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    /// The entity id, used by link setup.
    pub id: u32,
    /// The entity name (`"s5k3m5 20-002d"`, `"msm_csiphy0"`).
    pub name: String,
    /// `MEDIA_ENT_F_*`.
    pub function: u32,
    /// The device node (`/dev/video0`, `/dev/v4l-subdev3`) if the entity has one.
    pub node: Option<PathBuf>,
}

/// One pad of an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    /// The graph id of the pad.
    pub id: u32,
    /// The entity it belongs to.
    pub entity: u32,
    /// Its index on the entity.
    pub index: u32,
    /// Whether data flows into it.
    pub sink: bool,
}

/// A data link between two pads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// The source pad.
    pub source: Pad,
    /// The sink pad.
    pub sink: Pad,
    /// Whether the link is enabled.
    pub enabled: bool,
    /// Whether the link can be changed.
    pub immutable: bool,
}

/// An open `/dev/mediaN` with its graph as read at open time.
#[derive(Debug)]
pub struct MediaDevice {
    file: File,
    /// Every entity.
    pub entities: Vec<Entity>,
    /// Every pad.
    pub pads: Vec<Pad>,
    /// Every data link (interface links are folded into [`Entity::node`]).
    pub links: Vec<Link>,
    /// Ancillary links (sensor entity, lens or flash entity).
    pub ancillary: Vec<(u32, u32)>,
}

fn open_rw(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

/// The `/dev` path of the character device `major:minor`.
fn devnode_path(major: u32, minor: u32) -> Option<PathBuf> {
    let uevent = fs::read_to_string(format!("/sys/dev/char/{major}:{minor}/uevent")).ok()?;
    let name = uevent.lines().find_map(|line| line.strip_prefix("DEVNAME="))?;
    Some(Path::new("/dev").join(name))
}

fn name_of(raw: &[u8]) -> String {
    let len = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..len]).into_owned()
}

impl MediaDevice {
    /// Opens `path` and reads its graph.
    ///
    /// # Errors
    ///
    /// Returns the open or `MEDIA_IOC_G_TOPOLOGY` error.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = open_rw(path.as_ref())?;
        let fd = file.as_fd();
        let mut topology = Topology::default();
        // SAFETY: MEDIA_IOC_G_TOPOLOGY takes a `struct media_v2_topology`; null array
        // pointers only ask for the counts.
        unsafe { ioctl(fd, MEDIA_IOC_G_TOPOLOGY, &mut topology)? };

        // The graph may grow between the two calls; leave room and retry if it did.
        loop {
            let zero_entity = RawEntity { id: 0, name: [0; 64], function: 0, flags: 0, reserved: [0; 5] };
            let zero_interface =
                RawInterface { id: 0, intf_type: 0, flags: 0, reserved: [0; 9], raw: [0; 16] };
            let mut entities = vec![zero_entity; topology.num_entities as usize + 4];
            let mut interfaces = vec![zero_interface; topology.num_interfaces as usize + 4];
            let mut pads = vec![RawPad::default(); topology.num_pads as usize + 8];
            let mut links = vec![RawLink::default(); topology.num_links as usize + 8];
            let mut full = Topology {
                num_entities: entities.len() as u32,
                ptr_entities: entities.as_mut_ptr() as u64,
                num_interfaces: interfaces.len() as u32,
                ptr_interfaces: interfaces.as_mut_ptr() as u64,
                num_pads: pads.len() as u32,
                ptr_pads: pads.as_mut_ptr() as u64,
                num_links: links.len() as u32,
                ptr_links: links.as_mut_ptr() as u64,
                ..Default::default()
            };
            // SAFETY: as above; each array holds the count it is passed with.
            match unsafe { ioctl(fd, MEDIA_IOC_G_TOPOLOGY, &mut full) } {
                Err(err) if err.raw_os_error() == Some(libc::ENOSPC) => {
                    topology = full;
                    continue;
                }
                result => result?,
            }
            entities.truncate(full.num_entities as usize);
            interfaces.truncate(full.num_interfaces as usize);
            pads.truncate(full.num_pads as usize);
            links.truncate(full.num_links as usize);
            return Ok(Self::from_raw(file, &entities, &interfaces, &pads, &links));
        }
    }

    fn from_raw(
        file: File,
        entities: &[RawEntity],
        interfaces: &[RawInterface],
        pads: &[RawPad],
        links: &[RawLink],
    ) -> Self {
        let mut entities: Vec<Entity> = entities
            .iter()
            .map(|raw| Entity {
                id: raw.id,
                name: name_of(&raw.name),
                function: raw.function,
                node: None,
            })
            .collect();
        let pads: Vec<Pad> = pads
            .iter()
            .map(|raw| Pad {
                id: raw.id,
                entity: raw.entity_id,
                index: raw.index,
                sink: raw.flags & PAD_FL_SINK != 0,
            })
            .collect();
        let mut data_links = Vec::new();
        let mut ancillary = Vec::new();
        for raw in links {
            match raw.flags & LNK_FL_LINK_TYPE {
                LNK_FL_ANCILLARY_LINK => ancillary.push((raw.source_id, raw.sink_id)),
                LNK_FL_INTERFACE_LINK => {
                    let Some(interface) = interfaces.iter().find(|i| i.id == raw.source_id) else {
                        continue;
                    };
                    if let Some(entity) = entities.iter_mut().find(|e| e.id == raw.sink_id) {
                        entity.node = devnode_path(interface.raw[0], interface.raw[1]);
                    }
                }
                LNK_FL_DATA_LINK => {
                    let pad = |id: u32| pads.iter().copied().find(|pad| pad.id == id);
                    if let (Some(source), Some(sink)) = (pad(raw.source_id), pad(raw.sink_id)) {
                        data_links.push(Link {
                            source,
                            sink,
                            enabled: raw.flags & LNK_FL_ENABLED != 0,
                            immutable: raw.flags & LNK_FL_IMMUTABLE != 0,
                        });
                    }
                }
                _ => {}
            }
        }
        Self { file, entities, pads, links: data_links, ancillary }
    }

    /// The lens (focus motor) entity linked to `sensor`.
    pub fn lens_of(&self, sensor: u32) -> Option<&Entity> {
        self.ancillary
            .iter()
            .filter(|(source, _)| *source == sensor)
            .filter_map(|(_, sink)| self.entity(*sink))
            .find(|entity| entity.function == FUNCTION_LENS)
    }

    /// The entity with id `id`.
    pub fn entity(&self, id: u32) -> Option<&Entity> {
        self.entities.iter().find(|entity| entity.id == id)
    }

    /// The entity called `name`.
    pub fn entity_named(&self, name: &str) -> Option<&Entity> {
        self.entities.iter().find(|entity| entity.name == name)
    }

    /// The data links leaving `entity`.
    pub fn links_from(&self, entity: u32) -> impl Iterator<Item = &Link> {
        self.links.iter().filter(move |link| link.source.entity == entity)
    }

    /// Enables or disables a link; immutable links are left alone.
    ///
    /// # Errors
    ///
    /// Returns the `MEDIA_IOC_SETUP_LINK` error (`EBUSY` while streaming).
    pub fn set_link(&mut self, link: &Link, enabled: bool) -> io::Result<()> {
        if link.immutable {
            return Ok(());
        }
        let mut desc = LinkDesc {
            source: PadDesc {
                entity: link.source.entity,
                index: link.source.index as u16,
                ..Default::default()
            },
            sink: PadDesc {
                entity: link.sink.entity,
                index: link.sink.index as u16,
                flags: PAD_FL_SINK,
                ..Default::default()
            },
            flags: u32::from(enabled) * LNK_FL_ENABLED,
            ..Default::default()
        };
        // SAFETY: MEDIA_IOC_SETUP_LINK takes a `struct media_link_desc`.
        unsafe { ioctl(self.file.as_fd(), MEDIA_IOC_SETUP_LINK, &mut desc)? };
        if let Some(known) = self.links.iter_mut().find(|known| {
            known.source.id == link.source.id && known.sink.id == link.sink.id
        }) {
            known.enabled = enabled;
        }
        Ok(())
    }

    /// Disables every link that can be disabled, so a new pipeline starts clean.
    ///
    /// # Errors
    ///
    /// Returns the first `MEDIA_IOC_SETUP_LINK` error.
    pub fn reset_links(&mut self) -> io::Result<()> {
        let enabled: Vec<Link> =
            self.links.iter().copied().filter(|link| link.enabled && !link.immutable).collect();
        for link in enabled {
            self.set_link(&link, false)?;
        }
        Ok(())
    }

    /// Prints the graph like `media-ctl -p` does, without the formats.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for entity in &self.entities {
            out += &format!("- entity {}: {} (function {:#x})", entity.id, entity.name, entity.function);
            if let Some(node) = &entity.node {
                out += &format!(" {}", node.display());
            }
            out.push('\n');
            for pad in self.pads.iter().filter(|pad| pad.entity == entity.id) {
                out += &format!("\tpad{}: {}\n", pad.index, if pad.sink { "Sink" } else { "Source" });
                for link in &self.links {
                    let (other, arrow) = if link.source.id == pad.id {
                        (link.sink, "->")
                    } else if link.sink.id == pad.id {
                        (link.source, "<-")
                    } else {
                        continue;
                    };
                    let name = self.entity(other.entity).map_or("?", |e| e.name.as_str());
                    let mut flags = Vec::new();
                    if link.enabled {
                        flags.push("ENABLED");
                    }
                    if link.immutable {
                        flags.push("IMMUTABLE");
                    }
                    out += &format!("\t\t{arrow} \"{name}\":{} [{}]\n", other.index, flags.join(","));
                }
            }
        }
        out
    }
}

/// One entity a stream crosses, with the pads it enters and leaves by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    /// The entity.
    pub entity: Entity,
    /// The pad the stream enters by (none on the sensor).
    pub sink: Option<u32>,
    /// The pad the stream leaves by (none on the video node).
    pub source: Option<u32>,
}

/// A capture path from a sensor to a video node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The entities from the sensor to the video node.
    pub hops: Vec<Hop>,
    /// The links between them.
    pub links: Vec<Link>,
}

impl Route {
    /// The capture node at the end.
    pub fn video_node(&self) -> Option<&Path> {
        self.hops.last()?.entity.node.as_deref()
    }

    /// The sensor's sub-device node.
    pub fn sensor_node(&self) -> Option<&Path> {
        self.hops.first()?.entity.node.as_deref()
    }

    /// Whether `other` shares an entity other than the sensor with this route.
    pub fn conflicts_with(&self, other: &Route) -> bool {
        self.hops[1..].iter().any(|hop| other.hops[1..].iter().any(|o| o.entity.id == hop.entity.id))
    }
}

impl MediaDevice {
    /// The first path from `sensor` to a `/dev/video` node, depth first. Links are tried
    /// enabled first, then in graph order; entities `avoid` rejects are skipped (CAMSS:
    /// the PIX path, which needs an ISP setup, leaving the raw RDI paths).
    pub fn route(&self, sensor: u32, avoid: impl Fn(&Entity) -> bool) -> Option<Route> {
        let start = self.entity(sensor)?.clone();
        let mut route = Route {
            hops: vec![Hop { entity: start, sink: None, source: None }],
            links: Vec::new(),
        };
        self.extend_route(&mut route, &avoid).then_some(route)
    }

    fn extend_route(&self, route: &mut Route, avoid: &dyn Fn(&Entity) -> bool) -> bool {
        let current = route.hops.last().expect("a route starts at the sensor").entity.clone();
        let is_video = current
            .node
            .as_ref()
            .and_then(|node| node.file_name()?.to_str())
            .is_some_and(|name| name.starts_with("video"));
        if is_video && route.hops.len() > 1 {
            return true;
        }
        let mut links: Vec<Link> = self.links_from(current.id).copied().collect();
        links.sort_by_key(|link| !link.enabled);
        for link in links {
            let Some(next) = self.entity(link.sink.entity) else { continue };
            if avoid(next) || route.hops.iter().any(|hop| hop.entity.id == next.id) {
                continue;
            }
            route.hops.last_mut().expect("not empty").source = Some(link.source.index);
            route.hops.push(Hop { entity: next.clone(), sink: Some(link.sink.index), source: None });
            route.links.push(link);
            if self.extend_route(route, avoid) {
                return true;
            }
            route.hops.pop();
            route.links.pop();
        }
        route.hops.last_mut().expect("not empty").source = None;
        false
    }

    /// Enables the links of `route` and sets `format` on every pad it crosses after the
    /// sensor (raw paths pass the sensor format through unchanged).
    ///
    /// # Errors
    ///
    /// Returns the first link or format error, naming the entity.
    pub fn configure(&mut self, route: &Route, format: BusFormat) -> io::Result<()> {
        // Always enable: `route` may be older than the last reset_links().
        for link in &route.links {
            self.set_link(link, true)?;
        }
        for hop in &route.hops[1..] {
            let Some(node) = &hop.entity.node else { continue };
            if node.file_name().and_then(|name| name.to_str()).is_some_and(|n| n.starts_with("video")) {
                continue;
            }
            let subdev = Subdev::open(node)?;
            for pad in [hop.sink, hop.source].into_iter().flatten() {
                let set = subdev.set_format(pad, format).map_err(|err| {
                    io::Error::new(err.kind(), format!("{} pad {pad}: {err}", hop.entity.name))
                })?;
                if (set.code, set.width, set.height) != (format.code, format.width, format.height) {
                    return Err(io::Error::other(format!(
                        "{} pad {pad}: wanted {:#x} {}x{}, got {:#x} {}x{}",
                        hop.entity.name, format.code, format.width, format.height, set.code,
                        set.width, set.height
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A media bus frame format (`struct v4l2_mbus_framefmt`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusFormat {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `MEDIA_BUS_FMT_*`.
    pub code: u32,
    /// `V4L2_FIELD_*`.
    pub field: u32,
    colorspace: u32,
    ycbcr_enc: u16,
    quantization: u16,
    xfer_func: u16,
    flags: u16,
    reserved: [u16; 10],
}

impl BusFormat {
    /// A progressive format of `code` at `width` x `height`.
    pub fn new(code: u32, width: u32, height: u32) -> Self {
        Self { width, height, code, field: FIELD_NONE, ..Default::default() }
    }
}

const FIELD_NONE: u32 = 1;

#[repr(C)]
#[derive(Default)]
struct SubdevFormat {
    which: u32,
    pad: u32,
    format: BusFormat,
    stream: u32,
    reserved: [u32; 7],
}

#[repr(C)]
#[derive(Default)]
struct MbusCodeEnum {
    pad: u32,
    index: u32,
    code: u32,
    which: u32,
    flags: u32,
    stream: u32,
    reserved: [u32; 6],
}

#[repr(C)]
#[derive(Default)]
struct FrameSizeEnum {
    index: u32,
    pad: u32,
    code: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
    which: u32,
    stream: u32,
    reserved: [u32; 7],
}

#[repr(C)]
#[derive(Default)]
struct QueryCtrl {
    id: u32,
    kind: u32,
    name: [u8; 32],
    minimum: i32,
    maximum: i32,
    step: i32,
    default_value: i32,
    flags: u32,
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct Control {
    id: u32,
    value: i32,
}

#[repr(C, packed)]
#[derive(Default)]
struct ExtControl {
    id: u32,
    size: u32,
    reserved2: u32,
    value: i64,
}

#[repr(C)]
struct ExtControls {
    which: u32,
    count: u32,
    error_idx: u32,
    request_fd: i32,
    reserved: u32,
    controls: *mut ExtControl,
}

const VIDIOC_SUBDEV_ENUM_MBUS_CODE: u32 = ioc(READ | WRITE, b'V', 2, size_of::<MbusCodeEnum>());
const VIDIOC_SUBDEV_G_FMT: u32 = ioc(READ | WRITE, b'V', 4, size_of::<SubdevFormat>());
const VIDIOC_SUBDEV_S_FMT: u32 = ioc(READ | WRITE, b'V', 5, size_of::<SubdevFormat>());
const VIDIOC_G_CTRL: u32 = ioc(READ | WRITE, b'V', 27, size_of::<Control>());
const VIDIOC_S_CTRL: u32 = ioc(READ | WRITE, b'V', 28, size_of::<Control>());
const VIDIOC_QUERYCTRL: u32 = ioc(READ | WRITE, b'V', 36, size_of::<QueryCtrl>());
const VIDIOC_G_EXT_CTRLS: u32 = ioc(READ | WRITE, b'V', 71, size_of::<ExtControls>());
const VIDIOC_SUBDEV_ENUM_FRAME_SIZE: u32 =
    ioc(READ | WRITE, b'V', 74, size_of::<FrameSizeEnum>());

const FORMAT_ACTIVE: u32 = 1;
const CTRL_FLAG_DISABLED: u32 = 0x0001;

/// `V4L2_CID_EXPOSURE`, in lines on raw sensors.
pub const CID_EXPOSURE: u32 = 0x0098_0911;
/// `V4L2_CID_VBLANK`, in lines.
pub const CID_VBLANK: u32 = 0x009e_0901;
/// `V4L2_CID_HBLANK`, in pixels.
pub const CID_HBLANK: u32 = 0x009e_0902;
/// `V4L2_CID_ANALOGUE_GAIN`, in sensor-specific codes.
pub const CID_ANALOGUE_GAIN: u32 = 0x009e_0903;
/// `V4L2_CID_PIXEL_RATE`, pixels per second (64-bit).
pub const CID_PIXEL_RATE: u32 = 0x009f_0902;
/// `V4L2_CID_FOCUS_ABSOLUTE`, on lens drivers.
pub const CID_FOCUS_ABSOLUTE: u32 = 0x009a_090a;

/// The range of an integer control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRange {
    /// Lowest value.
    pub min: i32,
    /// Highest value.
    pub max: i32,
    /// Step between values.
    pub step: i32,
    /// The driver's default.
    pub default: i32,
}

/// An open V4L2 sub-device node.
#[derive(Debug)]
pub struct Subdev {
    file: File,
}

impl Subdev {
    /// Opens `path` (`/dev/v4l-subdevN`).
    ///
    /// # Errors
    ///
    /// Returns the open error.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self { file: open_rw(path.as_ref())? })
    }

    /// The active format of `pad`.
    ///
    /// # Errors
    ///
    /// Returns the ioctl error.
    pub fn format(&self, pad: u32) -> io::Result<BusFormat> {
        let mut format = SubdevFormat { which: FORMAT_ACTIVE, pad, ..Default::default() };
        // SAFETY: VIDIOC_SUBDEV_G_FMT takes a `struct v4l2_subdev_format`.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_SUBDEV_G_FMT, &mut format)? };
        Ok(format.format)
    }

    /// Sets the active format of `pad` and returns what the driver chose.
    ///
    /// # Errors
    ///
    /// Returns the ioctl error.
    pub fn set_format(&self, pad: u32, format: BusFormat) -> io::Result<BusFormat> {
        let mut request = SubdevFormat { which: FORMAT_ACTIVE, pad, format, ..Default::default() };
        // SAFETY: VIDIOC_SUBDEV_S_FMT takes a `struct v4l2_subdev_format`.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_SUBDEV_S_FMT, &mut request)? };
        Ok(request.format)
    }

    /// The media bus codes of `pad`.
    ///
    /// # Errors
    ///
    /// Returns an ioctl error other than the one that ends the list.
    pub fn codes(&self, pad: u32) -> io::Result<Vec<u32>> {
        let mut codes = Vec::new();
        for index in 0.. {
            let mut desc = MbusCodeEnum { pad, index, which: FORMAT_ACTIVE, ..Default::default() };
            // SAFETY: VIDIOC_SUBDEV_ENUM_MBUS_CODE takes a `struct v4l2_subdev_mbus_code_enum`.
            match unsafe { ioctl(self.file.as_fd(), VIDIOC_SUBDEV_ENUM_MBUS_CODE, &mut desc) } {
                Ok(()) => codes.push(desc.code),
                Err(err) if err.raw_os_error() == Some(libc::EINVAL) => break,
                Err(err) => return Err(err),
            }
        }
        Ok(codes)
    }

    /// The discrete frame sizes of `code` on `pad` (the largest of each range).
    ///
    /// # Errors
    ///
    /// Returns an ioctl error other than the one that ends the list.
    pub fn sizes(&self, pad: u32, code: u32) -> io::Result<Vec<(u32, u32)>> {
        let mut sizes = Vec::new();
        for index in 0.. {
            let mut desc =
                FrameSizeEnum { index, pad, code, which: FORMAT_ACTIVE, ..Default::default() };
            // SAFETY: VIDIOC_SUBDEV_ENUM_FRAME_SIZE takes a `struct v4l2_subdev_frame_size_enum`.
            match unsafe { ioctl(self.file.as_fd(), VIDIOC_SUBDEV_ENUM_FRAME_SIZE, &mut desc) } {
                Ok(()) => sizes.push((desc.max_width, desc.max_height)),
                Err(err) if err.raw_os_error() == Some(libc::EINVAL) => break,
                Err(err) => return Err(err),
            }
        }
        Ok(sizes)
    }

    /// The range of control `id`, `None` if the device has no such (enabled) control.
    ///
    /// # Errors
    ///
    /// Returns an ioctl error other than `EINVAL`.
    pub fn control_range(&self, id: u32) -> io::Result<Option<ControlRange>> {
        let mut query = QueryCtrl { id, ..Default::default() };
        // SAFETY: VIDIOC_QUERYCTRL takes a `struct v4l2_queryctrl`.
        match unsafe { ioctl(self.file.as_fd(), VIDIOC_QUERYCTRL, &mut query) } {
            Ok(()) if query.flags & CTRL_FLAG_DISABLED == 0 => Ok(Some(ControlRange {
                min: query.minimum,
                max: query.maximum,
                step: query.step.max(1),
                default: query.default_value,
            })),
            Ok(()) => Ok(None),
            Err(err) if err.raw_os_error() == Some(libc::EINVAL) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The value of a 32-bit control.
    ///
    /// # Errors
    ///
    /// Returns the ioctl error.
    pub fn control(&self, id: u32) -> io::Result<i32> {
        let mut control = Control { id, value: 0 };
        // SAFETY: VIDIOC_G_CTRL takes a `struct v4l2_control`.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_G_CTRL, &mut control)? };
        Ok(control.value)
    }

    /// The value of a 64-bit control (`V4L2_CID_PIXEL_RATE`).
    ///
    /// # Errors
    ///
    /// Returns the ioctl error.
    pub fn control64(&self, id: u32) -> io::Result<i64> {
        let mut control = ExtControl { id, ..Default::default() };
        let mut controls = ExtControls {
            which: 0,
            count: 1,
            error_idx: 0,
            request_fd: 0,
            reserved: 0,
            controls: &mut control,
        };
        // SAFETY: VIDIOC_G_EXT_CTRLS takes a `struct v4l2_ext_controls` whose array holds
        // `count` controls; `control` outlives the call.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_G_EXT_CTRLS, &mut controls)? };
        Ok(control.value)
    }

    /// Sets a 32-bit control and returns the value the driver kept.
    ///
    /// # Errors
    ///
    /// Returns the ioctl error.
    pub fn set_control(&self, id: u32, value: i32) -> io::Result<i32> {
        let mut control = Control { id, value };
        // SAFETY: VIDIOC_S_CTRL takes a `struct v4l2_control`.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_S_CTRL, &mut control)? };
        Ok(control.value)
    }
}

/// `MEDIA_BUS_FMT_S*10_1X10` codes and their Bayer order.
pub const BAYER10_CODES: [(u32, BayerOrder); 4] = [
    (0x3007, BayerOrder::Bggr),
    (0x300e, BayerOrder::Gbrg),
    (0x300a, BayerOrder::Grbg),
    (0x300f, BayerOrder::Rggb),
];

/// The colour of the top-left 2x2 block of a Bayer mosaic, row by row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BayerOrder {
    /// B G / G R.
    Bggr,
    /// G B / R G.
    Gbrg,
    /// G R / B G.
    Grbg,
    /// R G / G B.
    Rggb,
}

impl BayerOrder {
    /// The order of a 10-bit Bayer media bus code.
    pub fn of_code(code: u32) -> Option<Self> {
        BAYER10_CODES.iter().find(|(known, _)| *known == code).map(|(_, order)| *order)
    }

    /// The V4L2 pixel format of this order packed MIPI RAW10 (`V4L2_PIX_FMT_S*10P`).
    pub fn raw10_fourcc(self) -> u32 {
        let code = match self {
            Self::Bggr => *b"pBAA",
            Self::Gbrg => *b"pGAA",
            Self::Grbg => *b"pgAA",
            Self::Rggb => *b"pRAA",
        };
        u32::from_le_bytes(code)
    }

    /// The order of a `V4L2_PIX_FMT_S*10P` pixel format.
    pub fn of_raw10_fourcc(fourcc: u32) -> Option<Self> {
        [Self::Bggr, Self::Gbrg, Self::Grbg, Self::Rggb]
            .into_iter()
            .find(|order| order.raw10_fourcc() == fourcc)
    }

    /// The (row, column) of red within the 2x2 block.
    pub fn red_at(self) -> (usize, usize) {
        match self {
            Self::Rggb => (0, 0),
            Self::Grbg => (0, 1),
            Self::Gbrg => (1, 0),
            Self::Bggr => (1, 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_uapi() {
        assert_eq!(size_of::<Topology>(), 72);
        assert_eq!(size_of::<RawEntity>(), 96);
        assert_eq!(size_of::<RawInterface>(), 112);
        assert_eq!(size_of::<RawPad>(), 32);
        assert_eq!(size_of::<RawLink>(), 40);
        assert_eq!(size_of::<LinkDesc>(), 52);
        assert_eq!(size_of::<BusFormat>(), 48);
        assert_eq!(size_of::<SubdevFormat>(), 88);
        assert_eq!(size_of::<MbusCodeEnum>(), 48);
        assert_eq!(size_of::<FrameSizeEnum>(), 64);
        assert_eq!(size_of::<QueryCtrl>(), 68);
        assert_eq!(size_of::<ExtControl>(), 20);
        assert_eq!(size_of::<ExtControls>(), 32);
    }

    #[test]
    fn ioctl_numbers_match_uapi() {
        assert_eq!(MEDIA_IOC_G_TOPOLOGY, 0xc048_7c04);
        assert_eq!(MEDIA_IOC_SETUP_LINK, 0xc034_7c03);
        assert_eq!(VIDIOC_SUBDEV_G_FMT, 0xc058_5604);
        assert_eq!(VIDIOC_SUBDEV_S_FMT, 0xc058_5605);
        assert_eq!(VIDIOC_QUERYCTRL, 0xc044_5624);
        assert_eq!(VIDIOC_G_EXT_CTRLS, 0xc020_5647);
    }

    #[test]
    fn bayer_orders() {
        assert_eq!(BayerOrder::of_code(0x300e), Some(BayerOrder::Gbrg));
        assert_eq!(BayerOrder::Gbrg.raw10_fourcc(), u32::from_le_bytes(*b"pGAA"));
        assert_eq!(BayerOrder::of_raw10_fourcc(u32::from_le_bytes(*b"pRAA")), Some(BayerOrder::Rggb));
        assert_eq!(BayerOrder::Gbrg.red_at(), (1, 0));
    }
}

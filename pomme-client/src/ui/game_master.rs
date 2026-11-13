//! The screens of the blocks only a game master may use (an operator in
//! creative mode): vanilla `CommandBlockEditScreen`,
//! `JigsawBlockEditScreen` and `StructureBlockEditScreen`.
// TODO: the command field's suggestions (`CommandSuggestions`), the command
// block minecart's screen, the priority fields' tooltips, the structure
// name's own character filter, and a structure block's bounding box drawn
// in the world.

use azalea_protocol::packets::game::ServerboundGamePacket;
use azalea_protocol::packets::game::s_jigsaw_generate::ServerboundJigsawGenerate;
use azalea_protocol::packets::game::s_set_command_block::{
    Mode as WireMode, ServerboundSetCommandBlock,
};
use azalea_protocol::packets::game::s_set_jigsaw_block::ServerboundSetJigsawBlock;
use azalea_registry::identifier::Identifier;
use simdnbt::owned::NbtCompound;

use super::common::{BTN_H, FONT_SIZE, LabelScroll, WHITE, push_button, push_slider};
use super::container::push_transparent_background;
use super::form::Form;
use super::text_edit::TextInputEvent;
use crate::chat_component::Component;
use crate::position::BlockPos;
use crate::renderer::pipelines::menu_overlay::MenuElement;

/// A label over a field (`-6250336`).
const LABEL: [f32; 4] = [160.0 / 255.0, 160.0 / 255.0, 160.0 / 255.0, 1.0];

/// The blocks whose use opens one of these screens.
pub fn is_game_master_block(id: &str) -> bool {
    matches!(
        id,
        "command_block"
            | "chain_command_block"
            | "repeating_command_block"
            | "jigsaw"
            | "structure_block"
    )
}

/// What a frame of a screen did.
#[derive(Default)]
pub struct Frame {
    /// A widget was pressed (the click sound plays).
    pub clicked: bool,
    /// What the server is to be told.
    pub send: Vec<ServerboundGamePacket>,
    /// The same, for packets written here whole.
    pub send_raw: Vec<Vec<u8>>,
}

/// A frame's pointer, as a screen's widgets read it.
#[derive(Clone, Copy)]
pub struct Pointer {
    pub cursor: (f32, f32),
    pub pressed: bool,
    pub held: bool,
    pub shift: bool,
}

/// The screen a game master's block has open.
pub enum GameMasterUi {
    Command(CommandBlockUi),
    Jigsaw(JigsawUi),
    Structure(StructureUi),
}

impl GameMasterUi {
    /// The screen of the block `id` at `pos`, as its use opens it: a
    /// jigsaw's from its `orientation` and both it and a structure block's
    /// from the data the client holds for the block.
    pub fn open(
        id: &str,
        pos: BlockPos,
        orientation: Option<&str>,
        nbt: Option<&NbtCompound>,
        width: &dyn Fn(&str) -> f32,
    ) -> Option<Self> {
        match id {
            "command_block" | "chain_command_block" | "repeating_command_block" => {
                Some(Self::Command(CommandBlockUi::new(pos, width)))
            }
            "jigsaw" => Some(Self::Jigsaw(JigsawUi::new(pos, orientation, nbt, width))),
            "structure_block" => Some(Self::Structure(StructureUi::new(pos, nbt, width))),
            _ => None,
        }
    }

    pub fn pos(&self) -> BlockPos {
        match self {
            Self::Command(ui) => ui.pos,
            Self::Jigsaw(ui) => ui.pos,
            Self::Structure(ui) => ui.pos,
        }
    }

    pub fn close(&mut self) {
        match self {
            Self::Command(ui) => ui.closing = true,
            Self::Jigsaw(ui) => ui.closing = true,
            Self::Structure(ui) => ui.closing = true,
        }
    }

    pub fn closing(&self) -> bool {
        match self {
            Self::Command(ui) => ui.closing,
            Self::Jigsaw(ui) => ui.closing,
            Self::Structure(ui) => ui.closing,
        }
    }

    /// The block's data arrived from the server: `id` is the block there
    /// now, and `conditional` its state's property. Only a command block's
    /// screen waits on it.
    pub fn updated(
        &mut self,
        id: &str,
        conditional: bool,
        nbt: &NbtCompound,
        width: &dyn Fn(&str) -> f32,
    ) {
        if let Self::Command(ui) = self {
            ui.updated(id, conditional, nbt, width);
        }
    }

    pub fn handle_text_input(&mut self, events: &[TextInputEvent], width: &dyn Fn(&str) -> f32) {
        match self {
            Self::Command(ui) => ui.entered |= ui.form.handle_text_input(events, &[COMMAND], width),
            Self::Jigsaw(ui) => {
                ui.entered |= ui
                    .form
                    .handle_text_input(events, &[0, 1, 2, 3, 4, 5], width);
            }
            Self::Structure(ui) => {
                let order = ui.mode.fields();
                ui.entered |= ui.form.handle_text_input(events, order, width);
            }
        }
    }

    pub fn build(
        &mut self,
        elements: &mut Vec<MenuElement>,
        (screen_w, screen_h): (f32, f32),
        pointer: Pointer,
        gs: f32,
        text_width: &dyn Fn(&str, f32) -> f32,
    ) -> Frame {
        push_transparent_background(elements, screen_w, screen_h);
        let mut widgets = Widgets {
            elements,
            pointer,
            gs,
            text_width,
            middle: ((screen_w / gs).floor() / 2.0).floor(),
            height: (screen_h / gs).floor(),
            clicked: false,
        };
        let mut frame = Frame::default();
        match self {
            Self::Command(ui) => ui.build(&mut widgets, &mut frame),
            Self::Jigsaw(ui) => ui.build(&mut widgets, &mut frame),
            Self::Structure(ui) => ui.build(&mut widgets, &mut frame),
        }
        frame.clicked = widgets.clicked;
        frame
    }
}

fn translate(key: &str, fallback: &'static str) -> &'static str {
    crate::lang::translate(key).unwrap_or(fallback)
}

/// `CommonComponents.OPTION_ON` and `OPTION_OFF`.
fn on_off(on: bool) -> &'static str {
    if on {
        translate("options.on", "ON")
    } else {
        translate("options.off", "OFF")
    }
}

/// A screen's widgets as they are drawn: positions are GUI pixels, and
/// across the screen they count from its middle.
struct Widgets<'a> {
    elements: &'a mut Vec<MenuElement>,
    pointer: Pointer,
    gs: f32,
    text_width: &'a dyn Fn(&str, f32) -> f32,
    /// Half the screen's width.
    middle: f32,
    height: f32,
    clicked: bool,
}

impl Widgets<'_> {
    fn text(&mut self, x: f32, y: f32, text: &str, color: [f32; 4], centered: bool) {
        self.elements.push(MenuElement::Text {
            x: (self.middle + x) * self.gs,
            y: y * self.gs,
            text: text.to_owned(),
            scale: FONT_SIZE * self.gs,
            color,
            centered,
        });
    }

    /// A label starting `x` from the middle.
    fn label(&mut self, x: f32, y: f32, text: &str) {
        self.text(x, y, text, LABEL, false);
    }

    /// A label ending `right` from the middle.
    fn label_ending(&mut self, right: f32, y: f32, text: &str) {
        let width = ((self.text_width)(text, FONT_SIZE * self.gs) / self.gs).ceil();
        self.text(right - width, y, text, LABEL, false);
    }

    /// A button `x` from the middle; whether it was pressed.
    fn button(&mut self, [x, y, w]: [f32; 3], text: &str, enabled: bool) -> bool {
        let gs = self.gs;
        let over = push_button(
            self.elements,
            self.pointer.cursor,
            (self.middle + x) * gs,
            y * gs,
            w * gs,
            BTN_H * gs,
            gs,
            FONT_SIZE * gs,
            text,
            enabled,
        );
        let hit = over && self.pointer.pressed && enabled;
        self.clicked |= hit;
        hit
    }

    /// One of `form`'s fields, `x` from the middle.
    fn field(&mut self, form: &mut Form, field: usize, [x, y, w]: [f32; 3], editable: bool) {
        form.draw(
            self.elements,
            field,
            [self.middle + x, y, w, 20.0],
            editable,
            self.pointer.cursor,
            self.pointer.pressed,
            self.gs,
            self.text_width,
        );
    }

    fn width(&self) -> impl Fn(&str) -> f32 + '_ {
        |text| (self.text_width)(text, FONT_SIZE)
    }
}

/// `CommandBlockEntity.Mode`, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Sequence,
    Auto,
    Redstone,
}

impl Mode {
    /// The mode a command block of this kind runs in.
    fn of(block: &str) -> Self {
        match block {
            "chain_command_block" => Self::Sequence,
            "repeating_command_block" => Self::Auto,
            _ => Self::Redstone,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Sequence => Self::Auto,
            Self::Auto => Self::Redstone,
            Self::Redstone => Self::Sequence,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sequence => translate("advMode.mode.sequence", "Chain"),
            Self::Auto => translate("advMode.mode.auto", "Repeat"),
            Self::Redstone => translate("advMode.mode.redstone", "Impulse"),
        }
    }
}

/// The command block's fields: the command, and what it last put out.
const COMMAND: usize = 0;
const PREVIOUS: usize = 1;

/// Vanilla `CommandBlockEditScreen`.
pub struct CommandBlockUi {
    pos: BlockPos,
    form: Form,
    mode: Mode,
    conditional: bool,
    automatic: bool,
    track_output: bool,
    /// What the block last put out, shown while its output is tracked.
    last_output: String,
    /// The block's data has arrived; until then only Cancel answers
    /// (`enableControls`).
    loaded: bool,
    /// Enter was pressed since the last frame.
    entered: bool,
    closing: bool,
}

impl CommandBlockUi {
    fn new(pos: BlockPos, width: &dyn Fn(&str) -> f32) -> Self {
        let mut form = Form::new(&[(32500, 300.0), (32500, 276.0)]);
        form.set(PREVIOUS, "-", width);
        form.set_focus(Some(COMMAND));
        Self {
            pos,
            form,
            mode: Mode::Redstone,
            conditional: false,
            automatic: false,
            track_output: true,
            last_output: String::new(),
            loaded: false,
            entered: false,
            closing: false,
        }
    }

    /// `CommandBlockEditScreen.updateGui`, on the data `openCommandBlock`
    /// has the server send.
    fn updated(
        &mut self,
        block: &str,
        conditional: bool,
        nbt: &NbtCompound,
        width: &dyn Fn(&str) -> f32,
    ) {
        let command = nbt.string("Command").map(|command| command.to_str());
        self.form
            .set(COMMAND, command.as_deref().unwrap_or(""), width);
        self.track_output = nbt.byte("TrackOutput").is_none_or(|track| track != 0);
        self.automatic = nbt.byte("auto").is_some_and(|auto| auto != 0);
        self.last_output = nbt
            .get("LastOutput")
            .and_then(|output| Component::from_nbt_tag(output).ok())
            .map(|output| output.plain_text())
            .unwrap_or_default();
        self.mode = Mode::of(block);
        self.conditional = conditional;
        self.show_previous(width);
        self.loaded = true;
    }

    /// `updatePreviousOutput`: the last output, or a dash where none is
    /// kept.
    fn show_previous(&mut self, width: &dyn Fn(&str) -> f32) {
        let shown = if self.track_output {
            self.last_output.clone()
        } else {
            "-".to_owned()
        };
        self.form.set(PREVIOUS, &shown, width);
    }

    /// `populateAndSendPacket`.
    fn packet(&self) -> ServerboundGamePacket {
        ServerboundGamePacket::SetCommandBlock(ServerboundSetCommandBlock {
            pos: self.pos.into(),
            command: self.form.value(COMMAND).to_owned(),
            mode: match self.mode {
                Mode::Sequence => WireMode::Sequence,
                Mode::Auto => WireMode::Auto,
                Mode::Redstone => WireMode::Redstone,
            },
            track_output: self.track_output,
            conditional: self.conditional,
            automatic: self.automatic,
        })
    }

    fn build(&mut self, w: &mut Widgets, frame: &mut Frame) {
        let title = translate("advMode.setCommand", "Set Console Command for Block");
        w.text(0.0, 20.0, title, WHITE, true);
        w.label(
            -149.0,
            40.0,
            translate("advMode.command", "Console Command"),
        );
        w.field(&mut self.form, COMMAND, [-150.0, 50.0, 300.0], true);
        if !self.form.value(PREVIOUS).is_empty() {
            let previous = translate("advMode.previousOutput", "Previous Output");
            w.label(-149.0, 125.0, previous);
            w.field(&mut self.form, PREVIOUS, [-150.0, 135.0, 276.0], false);
        }

        let loaded = self.loaded;
        // Whether the block keeps its last output.
        let tracked = if self.track_output { "O" } else { "X" };
        if w.button([130.0, 135.0, 20.0], tracked, loaded) {
            self.track_output = !self.track_output;
            self.show_previous(&w.width());
        }
        if w.button([-154.0, 165.0, 100.0], self.mode.label(), loaded) {
            self.mode = self.mode.next();
        }
        let conditional = if self.conditional {
            translate("advMode.mode.conditional", "Conditional")
        } else {
            translate("advMode.mode.unconditional", "Unconditional")
        };
        self.conditional ^= w.button([-50.0, 165.0, 100.0], conditional, loaded);
        let automatic = if self.automatic {
            translate("advMode.mode.autoexec.bat", "Always Active")
        } else {
            translate("advMode.mode.redstoneTriggered", "Needs Redstone")
        };
        self.automatic ^= w.button([54.0, 165.0, 100.0], automatic, loaded);

        let buttons_y = (w.height / 4.0).floor() + 120.0 + 12.0;
        let done = w.button(
            [-154.0, buttons_y, 150.0],
            translate("gui.done", "Done"),
            loaded,
        );
        // `onDone`, from the button or Enter; Cancel sends nothing.
        if done || (std::mem::take(&mut self.entered) && self.loaded) {
            frame.send.push(self.packet());
            self.closing = true;
        }
        if w.button(
            [4.0, buttons_y, 150.0],
            translate("gui.cancel", "Cancel"),
            true,
        ) {
            self.closing = true;
        }
    }
}

/// Vanilla `Identifier.tryParse` succeeds: an optional namespace of
/// `[a-z0-9_.-]`, then a path that may also have slashes.
fn is_valid_identifier(text: &str) -> bool {
    let (namespace, path) = text.split_once(':').unwrap_or(("minecraft", text));
    let plain = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c);
    namespace.chars().all(plain) && path.chars().all(|c| plain(c) || c == '/')
}

/// A jigsaw's fields, down the screen.
const POOL: usize = 0;
const NAME: usize = 1;
const TARGET: usize = 2;
const FINAL_STATE: usize = 3;
const SELECTION_PRIORITY: usize = 4;
const PLACEMENT_PRIORITY: usize = 5;

/// Vanilla `JigsawBlockEditScreen`.
pub struct JigsawUi {
    pos: BlockPos,
    form: Form,
    /// `JigsawBlockEntity.JointType.ALIGNED`, not `ROLLABLE`.
    aligned: bool,
    /// The jigsaw faces up or down: only then has its joint a choice.
    vertical: bool,
    /// The levels to generate, as the slider stands (0 to 1) and counts.
    slider: f32,
    dragging: bool,
    keep_jigsaws: bool,
    entered: bool,
    closing: bool,
}

impl JigsawUi {
    fn new(
        pos: BlockPos,
        orientation: Option<&str>,
        nbt: Option<&NbtCompound>,
        width: &dyn Fn(&str) -> f32,
    ) -> Self {
        // `JigsawBlock.getFrontFacing`: the first of the orientation's two.
        let vertical = orientation.is_some_and(|o| o.starts_with("up") || o.starts_with("down"));
        let string = |key: &str, default: &str| {
            nbt.and_then(|nbt| nbt.string(key))
                .map_or_else(|| default.to_owned(), |value| value.to_str().into_owned())
        };
        let int = |key: &str| nbt.and_then(|nbt| nbt.int(key)).unwrap_or(0);
        let mut form = Form::new(&[
            (128, 300.0),
            (128, 300.0),
            (128, 300.0),
            (256, 300.0),
            (3, 98.0),
            (3, 98.0),
        ]);
        form.set(POOL, &string("pool", "minecraft:empty"), width);
        form.set(NAME, &string("name", "minecraft:empty"), width);
        form.set(TARGET, &string("target", "minecraft:empty"), width);
        form.set(FINAL_STATE, &string("final_state", "minecraft:air"), width);
        form.set(
            SELECTION_PRIORITY,
            &int("selection_priority").to_string(),
            width,
        );
        form.set(
            PLACEMENT_PRIORITY,
            &int("placement_priority").to_string(),
            width,
        );
        form.set_focus(Some(POOL));
        // `JigsawBlockEntity.getJoint`'s default: a jigsaw on its side is
        // aligned, one facing up or down rolls.
        let aligned = match nbt.and_then(|nbt| nbt.string("joint")) {
            Some(joint) => joint.to_str() == "aligned",
            None => !vertical,
        };
        Self {
            pos,
            form,
            aligned,
            vertical,
            slider: 0.0,
            dragging: false,
            keep_jigsaws: true,
            entered: false,
            closing: false,
        }
    }

    /// The slider's count: `Mth.floor(Mth.clampedLerp(0, 20, value))`.
    fn levels(&self) -> u32 {
        (f64::from(self.slider.clamp(0.0, 1.0)) * 20.0).floor() as u32
    }

    /// `updateValidity`: Done and Generate answer only to three ids.
    fn valid(&self) -> bool {
        [NAME, TARGET, POOL]
            .iter()
            .all(|field| is_valid_identifier(self.form.value(*field)))
    }

    /// `sendToServer`.
    fn packet(&self) -> ServerboundGamePacket {
        // `parseAsInt`: what is no number counts as none.
        let int = |field: usize| self.form.value(field).parse::<i32>().unwrap_or(0);
        ServerboundGamePacket::SetJigsawBlock(ServerboundSetJigsawBlock {
            pos: self.pos.into(),
            name: Identifier::new(self.form.value(NAME)),
            target: Identifier::new(self.form.value(TARGET)),
            pool: Identifier::new(self.form.value(POOL)),
            final_state: self.form.value(FINAL_STATE).to_owned(),
            joint: if self.aligned {
                "aligned".to_owned()
            } else {
                "rollable".to_owned()
            },
            selection_priority: int(SELECTION_PRIORITY),
            placement_priority: int(PLACEMENT_PRIORITY),
        })
    }

    fn build(&mut self, w: &mut Widgets, frame: &mut Frame) {
        let rows = [
            (POOL, "jigsaw_block.pool", "Target Pool:"),
            (NAME, "jigsaw_block.name", "Name:"),
            (TARGET, "jigsaw_block.target", "Target Name:"),
            (FINAL_STATE, "jigsaw_block.final_state", "Turns into:"),
        ];
        for (row, (field, key, fallback)) in rows.into_iter().enumerate() {
            let y = 10.0 + 35.0 * row as f32;
            w.label(-153.0, y, translate(key, fallback));
            w.field(&mut self.form, field, [-153.0, y + 10.0, 300.0], true);
        }
        let selection = translate("jigsaw_block.selection_priority", "Selection Priority:");
        w.label(-153.0, 150.0, selection);
        w.field(
            &mut self.form,
            SELECTION_PRIORITY,
            [-153.0, 160.0, 98.0],
            true,
        );
        let placement = translate("jigsaw_block.placement_priority", "Placement Priority:");
        w.label(-50.0, 150.0, placement);
        w.field(
            &mut self.form,
            PLACEMENT_PRIORITY,
            [-50.0, 160.0, 98.0],
            true,
        );
        if self.vertical {
            w.label(
                53.0,
                150.0,
                translate("jigsaw_block.joint_label", "Joint Type:"),
            );
            let joint = if self.aligned {
                translate("jigsaw_block.joint.aligned", "Aligned")
            } else {
                translate("jigsaw_block.joint.rollable", "Rollable")
            };
            self.aligned ^= w.button([54.0, 160.0, 100.0], joint, true);
        }

        // The levels to generate, by a slider.
        let levels = translate("jigsaw_block.levels", "Levels: %s").replacen(
            "%s",
            &self.levels().to_string(),
            1,
        );
        let gs = w.gs;
        let slid = push_slider(
            w.elements,
            w.pointer.cursor,
            w.pointer.pressed,
            w.pointer.held,
            (w.middle - 154.0) * gs,
            185.0 * gs,
            100.0 * gs,
            BTN_H * gs,
            gs,
            FONT_SIZE * gs,
            &levels,
            self.slider,
            true,
            false,
            false,
            self.dragging,
            &LabelScroll {
                text_width_fn: w.text_width,
                time_secs: 0.0,
            },
        );
        self.dragging = slid.dragging;
        if let Some(value) = slid.new_value {
            self.slider = value;
        }
        let keep = format!(
            "{}: {}",
            translate("jigsaw_block.keep_jigsaws", "Keep Jigsaws"),
            on_off(self.keep_jigsaws)
        );
        self.keep_jigsaws ^= w.button([-50.0, 185.0, 100.0], &keep, true);

        let valid = self.valid();
        let generate = translate("jigsaw_block.generate", "Generate");
        let generated = w.button([54.0, 185.0, 100.0], generate, valid);
        let done = w.button([-154.0, 210.0, 150.0], translate("gui.done", "Done"), valid);
        // Generate is Done, and then the order to generate.
        if generated || done || (std::mem::take(&mut self.entered) && valid) {
            frame.send.push(self.packet());
            self.closing = true;
        }
        if generated {
            frame.send.push(ServerboundGamePacket::JigsawGenerate(
                ServerboundJigsawGenerate {
                    pos: self.pos.into(),
                    levels: self.levels(),
                    keep_jigsaws: self.keep_jigsaws,
                },
            ));
        }
        if w.button([4.0, 210.0, 150.0], translate("gui.cancel", "Cancel"), true) {
            self.closing = true;
        }
    }
}

/// Vanilla `StructureMode`, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StructureMode {
    Save,
    Load,
    Corner,
    Data,
}

/// A structure block's fields.
const STRUCTURE_NAME: usize = 0;
const POSITION: usize = 1;
const SIZE: usize = 4;
const INTEGRITY: usize = 7;
const SEED: usize = 8;
const DATA: usize = 9;

impl StructureMode {
    fn of(name: &str) -> Self {
        match name {
            "SAVE" => Self::Save,
            "CORNER" => Self::Corner,
            "DATA" => Self::Data,
            _ => Self::Load,
        }
    }

    /// The mode's name in the game's files.
    fn key(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Load => "load",
            Self::Corner => "corner",
            Self::Data => "data",
        }
    }

    /// The button's next mode: data only with Shift held (the cycle's
    /// alternative list), or on from data itself.
    fn next(self, all: bool) -> Self {
        match self {
            Self::Save => Self::Load,
            Self::Load => Self::Corner,
            Self::Corner if all => Self::Data,
            Self::Corner | Self::Data => Self::Save,
        }
    }

    /// The fields the mode shows (`updateMode`), in Tab's order.
    fn fields(self) -> &'static [usize] {
        match self {
            Self::Save => &[0, 1, 2, 3, 4, 5, 6],
            Self::Load => &[0, 1, 2, 3, 7, 8],
            Self::Corner => &[0],
            Self::Data => &[9],
        }
    }
}

/// `DecimalFormat("0.0###")`: up to four decimals, and one at least.
fn format_integrity(integrity: f32) -> String {
    let text = format!("{integrity:.4}");
    let trimmed = text.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.to_owned()
    }
}

/// Vanilla `StructureBlockEditScreen`.
pub struct StructureUi {
    pos: BlockPos,
    form: Form,
    mode: StructureMode,
    /// `Mirror`'s ordinal: none, left to right, front to back.
    mirror: u8,
    /// `Rotation`'s ordinal: none, then a quarter turn clockwise at a time.
    rotation: u8,
    ignore_entities: bool,
    strict: bool,
    show_air: bool,
    show_bounding_box: bool,
    entered: bool,
    closing: bool,
}

impl StructureUi {
    fn new(pos: BlockPos, nbt: Option<&NbtCompound>, width: &dyn Fn(&str) -> f32) -> Self {
        let string = |key: &str| {
            nbt.and_then(|nbt| nbt.string(key))
                .map(|value| value.to_str().into_owned())
        };
        let int = |key: &str, default: i32| nbt.and_then(|nbt| nbt.int(key)).unwrap_or(default);
        let flag = |key: &str, default: bool| {
            nbt.and_then(|nbt| nbt.byte(key))
                .map_or(default, |value| value != 0)
        };
        let mut form = Form::new(&[
            (128, 300.0),
            (15, 80.0),
            (15, 80.0),
            (15, 80.0),
            (15, 80.0),
            (15, 80.0),
            (15, 80.0),
            (15, 80.0),
            (31, 80.0),
            (128, 240.0),
        ]);
        form.set(STRUCTURE_NAME, &string("name").unwrap_or_default(), width);
        // `StructureBlockEntity`'s own defaults: a block above, and no size.
        for (axis, (key, default)) in [("posX", 0), ("posY", 1), ("posZ", 0)]
            .into_iter()
            .enumerate()
        {
            form.set(POSITION + axis, &int(key, default).to_string(), width);
        }
        for (axis, key) in ["sizeX", "sizeY", "sizeZ"].into_iter().enumerate() {
            form.set(SIZE + axis, &int(key, 0).to_string(), width);
        }
        let integrity = nbt.and_then(|nbt| nbt.float("integrity")).unwrap_or(1.0);
        form.set(INTEGRITY, &format_integrity(integrity), width);
        let seed = nbt.and_then(|nbt| nbt.long("seed")).unwrap_or(0);
        form.set(SEED, &seed.to_string(), width);
        form.set(DATA, &string("metadata").unwrap_or_default(), width);
        form.set_focus(Some(STRUCTURE_NAME));
        Self {
            pos,
            form,
            mode: StructureMode::of(string("mode").as_deref().unwrap_or("LOAD")),
            mirror: match string("mirror").as_deref() {
                Some("LEFT_RIGHT") => 1,
                Some("FRONT_BACK") => 2,
                _ => 0,
            },
            rotation: match string("rotation").as_deref() {
                Some("CLOCKWISE_90") => 1,
                Some("CLOCKWISE_180") => 2,
                Some("COUNTERCLOCKWISE_90") => 3,
                _ => 0,
            },
            ignore_entities: flag("ignoreEntities", true),
            strict: flag("strict", false),
            show_air: flag("showair", false),
            show_bounding_box: flag("showboundingbox", true),
            entered: false,
            closing: false,
        }
    }

    /// `sendToServer`: everything the screen holds, with what the server
    /// is to do about it (0 to update, then save, load and scan).
    fn packet(&self, update: u32) -> Vec<u8> {
        // What is no number counts as none (`parseCoordinate` and its like).
        let int = |field: usize| self.form.value(field).parse::<i32>().unwrap_or(0) as i8;
        pomme_protocol::wire::encode_set_structure_block(
            &pomme_protocol::wire::StructureBlockUpdate {
                pos: (self.pos.x, self.pos.y, self.pos.z),
                update,
                mode: self.mode as u32,
                name: self.form.value(STRUCTURE_NAME),
                offset: [int(POSITION), int(POSITION + 1), int(POSITION + 2)],
                size: [int(SIZE), int(SIZE + 1), int(SIZE + 2)],
                mirror: u32::from(self.mirror),
                rotation: u32::from(self.rotation),
                data: self.form.value(DATA),
                integrity: self.form.value(INTEGRITY).parse().unwrap_or(1.0),
                seed: self.form.value(SEED).parse().unwrap_or(0),
                ignore_entities: self.ignore_entities,
                show_air: self.show_air,
                show_bounding_box: self.show_bounding_box,
                strict: self.strict,
            },
        )
    }

    fn build(&mut self, w: &mut Widgets, frame: &mut Frame) {
        use StructureMode::{Corner, Data, Load, Save};
        let mode = self.mode;
        let title = translate("block.minecraft.structure_block", "Structure Block");
        w.text(0.0, 10.0, title, WHITE, true);

        if mode != Data {
            let name = translate("structure_block.structure_name", "Structure Name");
            w.label(-153.0, 30.0, name);
            w.field(&mut self.form, STRUCTURE_NAME, [-152.0, 40.0, 300.0], true);
        }
        if matches!(mode, Save | Load) {
            let position = translate("structure_block.position", "Relative Position");
            w.label(-153.0, 70.0, position);
            for axis in 0..3 {
                let x = -152.0 + 80.0 * axis as f32;
                w.field(&mut self.form, POSITION + axis, [x, 80.0, 80.0], true);
            }
            let entities = translate("structure_block.include_entities", "Include Entities:");
            w.label_ending(154.0, 150.0, entities);
            self.ignore_entities ^=
                w.button([104.0, 160.0, 50.0], on_off(!self.ignore_entities), true);
        }
        let mut update = None;
        match mode {
            Save => {
                w.label(
                    -153.0,
                    110.0,
                    translate("structure_block.size", "Structure Size"),
                );
                for axis in 0..3 {
                    let x = -152.0 + 80.0 * axis as f32;
                    w.field(&mut self.form, SIZE + axis, [x, 120.0, 80.0], true);
                }
                let detect = translate(
                    "structure_block.detect_size",
                    "Detect Structure Size and Position:",
                );
                w.label_ending(154.0, 110.0, detect);
                let detect = translate("structure_block.button.detect_size", "DETECT");
                if w.button([104.0, 120.0, 50.0], detect, true) {
                    update = Some(3);
                }
                let air = translate("structure_block.show_air", "Show Invisible Blocks:");
                w.label_ending(154.0, 70.0, air);
                self.show_air ^= w.button([104.0, 80.0, 50.0], on_off(self.show_air), true);
                let save = translate("structure_block.button.save", "SAVE");
                if w.button([104.0, 185.0, 50.0], save, true) {
                    update = Some(1);
                }
            }
            Load => {
                let integrity =
                    translate("structure_block.integrity", "Structure Integrity and Seed");
                w.label(-153.0, 110.0, integrity);
                w.field(&mut self.form, INTEGRITY, [-152.0, 120.0, 80.0], true);
                w.field(&mut self.form, SEED, [-72.0, 120.0, 80.0], true);
                let strict = translate("structure_block.strict", "Strict Placement:");
                w.label_ending(154.0, 110.0, strict);
                self.strict ^= w.button([104.0, 120.0, 50.0], on_off(self.strict), true);
                let bounds = translate("structure_block.show_boundingbox", "Show Bounding Box:");
                w.label_ending(154.0, 70.0, bounds);
                self.show_bounding_box ^=
                    w.button([104.0, 80.0, 50.0], on_off(self.show_bounding_box), true);
                // The mirror, and the turn: the one in force cannot be
                // pressed again.
                let mirror = ["|", "< >", "^ v"][usize::from(self.mirror)];
                if w.button([-20.0, 185.0, 40.0], mirror, true) {
                    self.mirror = (self.mirror + 1) % 3;
                }
                let turns = [(-102.0, "0"), (-61.0, "90"), (21.0, "180"), (62.0, "270")];
                for (turn, (x, label)) in turns.into_iter().enumerate() {
                    if w.button([x, 185.0, 40.0], label, self.rotation != turn as u8) {
                        self.rotation = turn as u8;
                    }
                }
                let load = translate("structure_block.button.load", "LOAD");
                if w.button([104.0, 185.0, 50.0], load, true) {
                    update = Some(2);
                }
            }
            Corner => {}
            Data => {
                let data = translate("structure_block.custom_data", "Custom Data Tag Name");
                w.label(-153.0, 110.0, data);
                w.field(&mut self.form, DATA, [-152.0, 120.0, 240.0], true);
            }
        }

        // The mode, and what it is for.
        let mode_key = format!("structure_block.mode.{}", mode.key());
        let label = crate::lang::translate(&mode_key).unwrap_or(mode.key());
        if w.button([-154.0, 185.0, 50.0], label, true) {
            self.mode = mode.next(w.pointer.shift);
            // A field the new mode does not show keeps no focus.
            self.form.set_focus(self.mode.fields().first().copied());
        }
        let info_key = format!("structure_block.mode_info.{}", mode.key());
        w.label(
            -153.0,
            174.0,
            crate::lang::translate(&info_key).unwrap_or(""),
        );

        let done = w.button([-154.0, 210.0, 150.0], translate("gui.done", "Done"), true);
        if done || std::mem::take(&mut self.entered) {
            update = Some(0);
        }
        // Each of them sends the lot and closes the screen.
        if let Some(update) = update {
            frame.send_raw.push(self.packet(update));
            self.closing = true;
        }
        if w.button([4.0, 210.0, 150.0], translate("gui.cancel", "Cancel"), true) {
            self.closing = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use simdnbt::owned::{NbtCompound, NbtTag};

    use super::*;

    fn width(text: &str) -> f32 {
        text.len() as f32 * 6.0
    }

    const POS: BlockPos = BlockPos { x: 1, y: 2, z: 3 };

    /// The screen takes the block's data once the server sends it, and
    /// sends back what it then shows.
    #[test]
    fn a_command_block_shows_and_sends_its_data() {
        let mut ui = CommandBlockUi::new(POS, &width);
        assert!(!ui.loaded);
        assert_eq!(ui.form.value(PREVIOUS), "-");

        let mut nbt = NbtCompound::new();
        nbt.insert("Command", NbtTag::String("say hi".into()));
        nbt.insert("auto", NbtTag::Byte(1));
        nbt.insert("TrackOutput", NbtTag::Byte(1));
        nbt.insert("LastOutput", NbtTag::String("[@] hi".into()));
        ui.updated("chain_command_block", true, &nbt, &width);
        assert!(ui.loaded);
        assert_eq!(ui.form.value(COMMAND), "say hi");
        assert_eq!(ui.form.value(PREVIOUS), "[@] hi");
        assert_eq!(ui.mode, Mode::Sequence);

        // Output no longer tracked shows as a dash.
        ui.track_output = false;
        ui.show_previous(&width);
        assert_eq!(ui.form.value(PREVIOUS), "-");

        ui.mode = ui.mode.next();
        let ServerboundGamePacket::SetCommandBlock(packet) = ui.packet() else {
            panic!("a command block sends its own packet");
        };
        assert_eq!(packet.command, "say hi");
        assert_eq!(packet.mode, WireMode::Auto);
        assert!(packet.conditional && packet.automatic && !packet.track_output);
    }

    #[test]
    fn modes_cycle_in_vanillas_order() {
        assert_eq!(Mode::of("command_block"), Mode::Redstone);
        assert_eq!(Mode::of("repeating_command_block"), Mode::Auto);
        assert_eq!(Mode::Redstone.next(), Mode::Sequence);
        assert_eq!(Mode::Sequence.next().next(), Mode::Redstone);
    }

    #[test]
    fn identifiers_are_told_from_other_text() {
        for valid in ["minecraft:empty", "empty", "void_rush:arena/spawn-1", ""] {
            assert!(is_valid_identifier(valid), "{valid}");
        }
        for invalid in ["Minecraft:empty", "a b", "a:b:c", "name/space:path"] {
            assert!(!is_valid_identifier(invalid), "{invalid}");
        }
    }

    /// A jigsaw's screen starts from the block's data, takes a joint only
    /// when it faces up or down, and sends what it shows.
    #[test]
    fn a_jigsaw_shows_and_sends_its_data() {
        let mut nbt = NbtCompound::new();
        nbt.insert("pool", NbtTag::String("void_rush:rooms".into()));
        nbt.insert("name", NbtTag::String("void_rush:door".into()));
        nbt.insert("selection_priority", NbtTag::Int(3));
        let mut ui = JigsawUi::new(POS, Some("up_north"), Some(&nbt), &width);
        assert!(ui.vertical && !ui.aligned);
        assert_eq!(ui.form.value(TARGET), "minecraft:empty");
        assert_eq!(ui.form.value(FINAL_STATE), "minecraft:air");
        assert!(ui.valid());
        ui.slider = 0.5;
        assert_eq!(ui.levels(), 10);
        ui.slider = 1.0;
        assert_eq!(ui.levels(), 20);

        let ServerboundGamePacket::SetJigsawBlock(packet) = ui.packet() else {
            panic!("a jigsaw sends its own packet");
        };
        assert_eq!(packet.pool.to_string(), "void_rush:rooms");
        assert_eq!(packet.name.to_string(), "void_rush:door");
        assert_eq!(packet.joint, "rollable");
        assert_eq!(
            (packet.selection_priority, packet.placement_priority),
            (3, 0)
        );

        // One on its side is aligned, and a name that is no id stops Done.
        let mut side = JigsawUi::new(POS, Some("north_up"), None, &width);
        assert!(!side.vertical && side.aligned);
        side.form.set(NAME, "Not An Id", &width);
        assert!(!side.valid());
    }

    #[test]
    fn integrity_keeps_a_decimal_and_no_more_than_four() {
        assert_eq!(format_integrity(1.0), "1.0");
        assert_eq!(format_integrity(0.5), "0.5");
        assert_eq!(format_integrity(0.123_456), "0.1235");
    }

    /// A structure block's screen starts from the block's data, or the
    /// block's own defaults, and shows a mode's own fields.
    #[test]
    fn a_structure_block_shows_its_data() {
        let fresh = StructureUi::new(POS, None, &width);
        assert_eq!(fresh.mode, StructureMode::Load);
        assert_eq!(fresh.form.value(POSITION + 1), "1");
        assert_eq!(fresh.form.value(INTEGRITY), "1.0");
        assert!(fresh.ignore_entities && fresh.show_bounding_box && !fresh.strict);

        let mut nbt = NbtCompound::new();
        nbt.insert("name", NbtTag::String("void_rush:arena".into()));
        nbt.insert("mode", NbtTag::String("SAVE".into()));
        nbt.insert("sizeX", NbtTag::Int(12));
        nbt.insert("rotation", NbtTag::String("CLOCKWISE_180".into()));
        nbt.insert("mirror", NbtTag::String("FRONT_BACK".into()));
        nbt.insert("ignoreEntities", NbtTag::Byte(0));
        let ui = StructureUi::new(POS, Some(&nbt), &width);
        assert_eq!(ui.mode, StructureMode::Save);
        assert_eq!(ui.form.value(STRUCTURE_NAME), "void_rush:arena");
        assert_eq!(ui.form.value(SIZE), "12");
        assert_eq!((ui.rotation, ui.mirror), (2, 2));
        assert!(!ui.ignore_entities);

        // Data is reached only with Shift, and left for save.
        assert_eq!(StructureMode::Corner.next(false), StructureMode::Save);
        assert_eq!(StructureMode::Corner.next(true), StructureMode::Data);
        assert_eq!(StructureMode::Data.next(false), StructureMode::Save);
        assert_eq!(StructureMode::Load.fields(), [0, 1, 2, 3, 7, 8]);
    }
}

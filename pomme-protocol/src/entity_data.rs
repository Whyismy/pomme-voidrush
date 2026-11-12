//! `ClientboundSetEntityDataPacket`'s value list: each entry is an index, a
//! serializer id and that serializer's value, with no length prefix, so an
//! unknown serializer ends the packet (`SynchedEntityData.DataValue.read`).

use crate::codec::{DecodeResult, MAX_STRING_LENGTH, Reader};
use crate::item::{ItemCodec, ItemStack, PARTICLE, Shape};

/// `ClientboundSetEntityDataPacket.EOF_MARKER`.
const EOF_MARKER: u8 = 0xFF;

/// The entity variants the client resolves to textures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Cat,
    Cow,
    Wolf,
    Chicken,
}

/// How a serializer's value is read: decoded for the ones the client uses,
/// otherwise validated and skipped by shape.
#[derive(Debug)]
pub enum Serializer {
    Byte,
    Int,
    Long,
    Float,
    Bool,
    /// `COMPONENT`, kept as NBT bytes for display entities' text.
    Component,
    OptionalComponent,
    ItemStack,
    /// `BLOCK_STATE`: a state id in the session's id space.
    BlockState,
    /// `OPTIONAL_BLOCK_STATE`: a state id, 0 for none.
    OptionalBlockState,
    /// `VECTOR3`: `ByteBufCodecs.VECTOR3F`.
    Vector3,
    /// `QUATERNION`: `ByteBufCodecs.QUATERNIONF`, x y z w.
    Quaternion,
    OptionalBlockPos,
    VillagerData,
    Pose,
    Variant(Variant),
    /// `RESOLVABLE_PROFILE`: a mannequin's skin.
    Profile,
    /// `ROTATIONS`: an armor stand's part poses, x y z degrees.
    Rotations,
    /// `PAINTING_VARIANT`: a holder, by registry id or spelled out.
    Painting,
    /// `PARTICLE`: one particle with its options, kept as its bytes.
    Particle,
    Skip(Shape),
}

/// One decoded entry's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Byte(u8),
    Int(i32),
    Long(i64),
    Float(f32),
    Bool(bool),
    /// A text component's NBT bytes.
    Component(Box<[u8]>),
    /// A text component's NBT bytes.
    OptionalComponent(Option<Box<[u8]>>),
    BlockState(u32),
    /// A minecart's custom display block.
    OptionalBlockState(Option<u32>),
    Vector3([f32; 3]),
    Quaternion([f32; 4]),
    Item(Option<ItemStack>),
    OptionalBlockPos(Option<(i32, i32, i32)>),
    VillagerData {
        kind: u32,
        profession: u32,
        level: u32,
    },
    /// `Pose.id`.
    Pose(u32),
    /// A registry id.
    Variant(Variant, u32),
    Profile(Box<Profile>),
    Rotations([f32; 3]),
    Painting(Painting),
    /// A particle's type id and options, as sent.
    Particle(Box<[u8]>),
    /// A value the client doesn't read.
    Other,
}

/// A painting's variant (`Holder<PaintingVariant>`): an entry of the
/// `painting_variant` registry, or one the server spells out.
#[derive(Clone, Debug, PartialEq)]
pub enum Painting {
    Id(u32),
    Direct {
        /// In blocks.
        width: u32,
        height: u32,
        asset_id: String,
    },
}

/// What a `ResolvableProfile` says about a skin: the profile (a full one
/// names and identifies a player; a partial one may name either) with its
/// `textures` property, and `PlayerSkin.Patch`'s body texture and model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub name: Option<String>,
    pub id: Option<u128>,
    /// The `textures` property's base64 value.
    pub textures: Option<String>,
    /// A texture asset id replacing the skin.
    pub body: Option<String>,
    /// The model the patch forces: slim (`true`) or wide.
    pub slim: Option<bool>,
}

/// `ResolvableProfile.STREAM_CODEC`: `Either(GAME_PROFILE, Partial)`, then
/// `PlayerSkin.Patch.STREAM_CODEC` (body, cape and elytra assets, model).
fn read_profile(r: &mut Reader) -> DecodeResult<Profile> {
    let (name, id) = if r.bool()? {
        let id = r.uuid()?;
        (Some(r.utf(16)?.to_owned()), Some(id))
    } else {
        let name = r.optional(|r| Ok(r.utf(16)?.to_owned()))?;
        (name, r.optional(Reader::uuid)?)
    };
    let mut textures = None;
    for _ in 0..r.len(16)? {
        let key = r.utf(64)?;
        let value = r.utf(32767)?;
        r.optional(|r| r.utf(1024).map(drop))?; // signature
        if key == "textures" {
            textures = Some(value.to_owned());
        }
    }
    let body = r.optional(|r| Ok(r.identifier()?.to_owned()))?;
    r.optional(|r| r.identifier().map(drop))?; // cape
    r.optional(|r| r.identifier().map(drop))?; // elytra
    let slim = r.optional(Reader::bool)?;
    Ok(Profile {
        name,
        id,
        textures,
        body,
        slim,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct DataValue {
    pub index: u8,
    pub value: Value,
}

use Serializer as S;
use Shape::*;

/// `ComponentSerialization.TRUSTED_STREAM_CODEC`: NBT.
const TEXT: Shape = Nbt;
/// A `holderRegistry` id.
const REGISTRY_ID: Shape = VarInt;
/// An `idMapper` enum.
const ENUM: Shape = VarInt;

/// The 26.2 serializers by id, in `EntityDataSerializers`' static-block
/// registration order (`EntityDataSerializers.java:166-210`), which differs
/// from the field order; `$extra` are a later version's appended ones.
macro_rules! serializers_26_2 {
    ($($extra:expr),*) => { &[
    S::Byte,
    S::Int,
    S::Long,
    S::Float,
    // STRING_UTF8
    S::Skip(Utf(MAX_STRING_LENGTH)),
    S::Component,
    S::OptionalComponent,
    S::ItemStack,
    S::Bool,
    // Rotations.java:18
    S::Rotations,
    // BlockPos
    S::Skip(Long),
    S::OptionalBlockPos,
    // Direction
    S::Skip(ENUM),
    // OPTIONAL_LIVING_ENTITY_REFERENCE: EntityReference.java:34
    S::Skip(Optional(&Uuid)),
    S::BlockState,
    // 0 for none, not a bool.
    S::OptionalBlockState,
    S::Particle,
    S::Skip(List(usize::MAX, &PARTICLE)),
    S::VillagerData,
    // OPTIONAL_UNSIGNED_INT: 0 for none, else the value + 1.
    S::Skip(VarInt),
    S::Pose,
    S::Variant(Variant::Cat),
    // CAT_SOUND_VARIANT
    S::Skip(REGISTRY_ID),
    S::Variant(Variant::Cow),
    // COW_SOUND_VARIANT
    S::Skip(REGISTRY_ID),
    S::Variant(Variant::Wolf),
    // WOLF_SOUND_VARIANT
    S::Skip(REGISTRY_ID),
    // FROG_VARIANT
    S::Skip(REGISTRY_ID),
    // PIG_VARIANT
    S::Skip(REGISTRY_ID),
    // PIG_SOUND_VARIANT
    S::Skip(REGISTRY_ID),
    S::Variant(Variant::Chicken),
    // CHICKEN_SOUND_VARIANT
    S::Skip(REGISTRY_ID),
    // ZOMBIE_NAUTILUS_VARIANT
    S::Skip(REGISTRY_ID),
    // OPTIONAL_GLOBAL_POS: GlobalPos.java:29, a dimension then a BlockPos.
    S::Skip(Optional(&Struct(&[Utf(MAX_STRING_LENGTH), Long]))),
    // PAINTING_VARIANT: PaintingVariant.java:30-32, a direct-capable holder.
    S::Painting,
    // SNIFFER_STATE: an idMapper VarInt, read like INT.
    S::Int,
    // ARMADILLO_STATE: an idMapper VarInt, read like INT.
    S::Int,
    // COPPER_GOLEM_STATE and WEATHERING_COPPER_STATE: idMapper VarInts.
    S::Int,
    S::Int,
    S::Vector3,
    S::Quaternion,
    S::Profile,
    // HUMANOID_ARM
    S::Skip(ENUM),
    $($extra),*
] };
}

pub static SERIALIZERS_26_2: &[Serializer] = serializers_26_2!();

/// 26.3 appended `DYE_COLOR` (`DyeColor.STREAM_CODEC`, an `idMapper` enum).
pub static SERIALIZERS_26_3: &[Serializer] = serializers_26_2!(S::Skip(ENUM));

impl ItemCodec {
    /// `ClientboundSetEntityDataPacket.unpack`: entries up to the end marker.
    pub fn read_entity_data(&self, r: &mut Reader) -> DecodeResult<Vec<DataValue>> {
        let mut values = Vec::new();
        loop {
            let index = r.u8()?;
            if index == EOF_MARKER {
                return Ok(values);
            }
            let serializer = r.varint()?;
            let serializer = self
                .entity_data_serializers()
                .get(serializer as usize)
                .ok_or_else(|| r.error("unknown entity data serializer"))?;
            let value = self.read_data_value(r, serializer)?;
            values.push(DataValue { index, value });
        }
    }

    fn read_data_value(
        &self,
        r: &mut Reader,
        serializer: &'static Serializer,
    ) -> DecodeResult<Value> {
        Ok(match serializer {
            S::Byte => Value::Byte(r.u8()?),
            S::Int => Value::Int(r.var_i32()?),
            S::Long => Value::Long(r.varlong()? as i64),
            S::Float => Value::Float(r.f32()?),
            S::Bool => Value::Bool(r.bool()?),
            S::Component => {
                let start = r.pos();
                self.skip(r, &TEXT)?;
                Value::Component(r.since(start).into())
            }
            S::BlockState => Value::BlockState(r.varint()?),
            S::OptionalBlockState => {
                let state = r.varint()?;
                Value::OptionalBlockState((state != 0).then_some(state))
            }
            S::Vector3 => Value::Vector3([r.f32()?, r.f32()?, r.f32()?]),
            S::Quaternion => Value::Quaternion([r.f32()?, r.f32()?, r.f32()?, r.f32()?]),
            S::OptionalComponent => Value::OptionalComponent(r.optional(|r| {
                let start = r.pos();
                self.skip(r, &TEXT)?;
                Ok(r.since(start).into())
            })?),
            S::ItemStack => Value::Item(self.read_stack(r)?),
            S::OptionalBlockPos => Value::OptionalBlockPos(r.optional(Reader::block_pos)?),
            S::VillagerData => Value::VillagerData {
                kind: r.varint()?,
                profession: r.varint()?,
                level: r.varint()?,
            },
            S::Pose => Value::Pose(r.varint()?),
            S::Variant(variant) => Value::Variant(*variant, r.varint()?),
            S::Profile => Value::Profile(Box::new(read_profile(r)?)),
            S::Rotations => Value::Rotations([r.f32()?, r.f32()?, r.f32()?]),
            S::Particle => {
                let start = r.pos();
                self.skip(r, &PARTICLE)?;
                Value::Particle(r.since(start).into())
            }
            // A holder: the registry id plus one, or zero and the variant
            // itself (width, height, asset, then a title and an author).
            S::Painting => Value::Painting(match r.varint()? {
                0 => {
                    let width = r.varint()?;
                    let height = r.varint()?;
                    let asset_id = r.utf(MAX_STRING_LENGTH)?.to_owned();
                    self.skip(r, &Optional(&TEXT))?;
                    self.skip(r, &Optional(&TEXT))?;
                    Painting::Direct {
                        width,
                        height,
                        asset_id,
                    }
                }
                id => Painting::Id(id - 1),
            }),
            S::Skip(shape) => {
                self.skip(r, shape)?;
                Value::Other
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::tests::{codec, id};
    use crate::registries::ClientRegistry;
    use crate::wire::write_varint;

    /// Entity data entries built field by field: (index, serializer, value).
    fn frame(entries: &[(u8, u32, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, serializer, value) in entries {
            out.push(*index);
            write_varint(&mut out, *serializer);
            out.extend_from_slice(value);
        }
        out.push(EOF_MARKER);
        out
    }

    fn read(bytes: &[u8]) -> DecodeResult<Vec<DataValue>> {
        let mut r = Reader::new(bytes);
        let values = codec().read_entity_data(&mut r)?;
        r.ensure_eof()?;
        Ok(values)
    }

    fn values(bytes: &[u8]) -> Vec<Value> {
        read(bytes)
            .expect("valid entity data")
            .into_iter()
            .map(|v| v.value)
            .collect()
    }

    #[test]
    fn a_mannequin_profile_carries_its_skin() {
        let serializer = SERIALIZERS_26_2
            .iter()
            .position(|s| matches!(s, Serializer::Profile))
            .unwrap() as u32;
        let utf = |out: &mut Vec<u8>, s: &str| {
            write_varint(out, s.len() as u32);
            out.extend_from_slice(s.as_bytes());
        };
        let mut full = vec![1]; // a full GameProfile
        full.extend_from_slice(&7u128.to_be_bytes());
        utf(&mut full, "Bramfang");
        full.push(1); // one property, signed
        utf(&mut full, "textures");
        utf(&mut full, "e30=");
        full.push(1);
        utf(&mut full, "sig");
        full.extend_from_slice(&[0, 0, 0, 1, 1]); // no assets; model: slim
        let mut partial = vec![0, 1]; // a partial profile: a name, no id
        utf(&mut partial, "Kaelith");
        partial.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let values = read(&frame(&[
            (17, serializer, &full),
            (17, serializer, &partial),
        ]))
        .unwrap();
        assert_eq!(
            values[0].value,
            Value::Profile(Box::new(Profile {
                name: Some("Bramfang".into()),
                id: Some(7),
                textures: Some("e30=".into()),
                body: None,
                slim: Some(true),
            }))
        );
        assert_eq!(
            values[1].value,
            Value::Profile(Box::new(Profile {
                name: Some("Kaelith".into()),
                ..Profile::default()
            }))
        );
    }

    #[test]
    fn table_covers_every_serializer() {
        assert_eq!(SERIALIZERS_26_2.len(), 43);
        assert_eq!(SERIALIZERS_26_3.len(), 44);
    }

    #[test]
    fn decodes_the_values_the_client_reads() {
        let packed = (5i64 << 38) | ((-3i64 & 0x3ff_ffff) << 12) | (-7i64 & 0xfff);
        let pos = [&[1u8][..], &packed.to_be_bytes()].concat();
        let bytes = frame(&[
            (0, 0, &[0x82]),
            (1, 1, &[0xAC, 0x02]),
            (2, 3, &1.5f32.to_be_bytes()),
            (3, 8, &[1]),
            (4, 6, &[1, 8, 0, 2, b'h', b'i']),
            (5, 6, &[0]),
            (6, 20, &[5]),
            (7, 11, &pos),
            (8, 18, &[2, 3, 4]),
            (9, 23, &[7]),
            (10, 7, &[0]),
        ]);
        assert_eq!(
            values(&bytes),
            [
                Value::Byte(0x82),
                Value::Int(300),
                Value::Float(1.5),
                Value::Bool(true),
                Value::OptionalComponent(Some([8, 0, 2, b'h', b'i'].into())),
                Value::OptionalComponent(None),
                Value::Pose(5),
                Value::OptionalBlockPos(Some((5, -7, -3))),
                Value::VillagerData {
                    kind: 2,
                    profession: 3,
                    level: 4,
                },
                Value::Variant(Variant::Cow, 7),
                Value::Item(None),
            ]
        );
    }

    /// `CopperGolem`'s weathering (16) WEATHERED and state (17) DROPPING_ITEM,
    /// by vanilla's encoder.
    #[test]
    fn copper_golem_states_match_vanilla_encoder() {
        let payload = crate::vanilla_fixtures::case("set_entity_data/copper_golem");
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().unwrap(), 7);
        let values = codec().read_entity_data(&mut r).expect("valid entity data");
        r.ensure_eof().unwrap();
        assert_eq!(
            values,
            [
                DataValue {
                    index: 16,
                    value: Value::Int(2),
                },
                DataValue {
                    index: 17,
                    value: Value::Int(3),
                },
            ]
        );
    }

    /// `Sniffer.DATA_STATE` (index 18) set to DIGGING by vanilla's encoder.
    #[test]
    fn sniffer_state_matches_vanilla_encoder() {
        let payload = crate::vanilla_fixtures::case("set_entity_data/sniffer_state");
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().unwrap(), 7);
        let values = codec().read_entity_data(&mut r).expect("valid entity data");
        r.ensure_eof().unwrap();
        assert_eq!(
            values,
            [DataValue {
                index: 18,
                value: Value::Int(5),
            }]
        );
    }

    /// `Armadillo.ARMADILLO_STATE` (index 18) set to UNROLLING by vanilla's
    /// encoder.
    #[test]
    fn armadillo_state_matches_vanilla_encoder() {
        let payload = crate::vanilla_fixtures::case("set_entity_data/armadillo_state");
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().unwrap(), 7);
        let values = codec().read_entity_data(&mut r).expect("valid entity data");
        r.ensure_eof().unwrap();
        assert_eq!(
            values,
            [DataValue {
                index: 18,
                value: Value::Int(3),
            }]
        );
    }

    #[test]
    fn decodes_an_item_stack() {
        let stone = id(ClientRegistry::Item, "stone");
        let mut value = vec![3];
        write_varint(&mut value, stone);
        value.extend([0, 0]);
        let [Value::Item(Some(stack))] = &values(&frame(&[(8, 7, &value)]))[..] else {
            panic!("expected one stack");
        };
        assert_eq!((stack.item, stack.count), (stone, 3));
    }

    #[test]
    fn skips_values_by_shape() {
        let dust = id(ClientRegistry::ParticleType, "dust");
        let flame = id(ClientRegistry::ParticleType, "flame");
        let block_source = id(ClientRegistry::PositionSourceType, "block");
        let vibration = id(ClientRegistry::ParticleType, "vibration");
        let mut particles = vec![3];
        write_varint(&mut particles, dust);
        particles.extend([0, 0, 0, 1, 0x3F, 0x80, 0, 0]);
        write_varint(&mut particles, flame);
        write_varint(&mut particles, vibration);
        write_varint(&mut particles, block_source);
        particles.extend([0; 8]);
        particles.push(20);
        let painting = [0, 2, 1, 1, b'a', 1, 8, 0, 1, b't', 0];
        let bytes = frame(&[
            // OPTIONAL_BLOCK_STATE and OPTIONAL_UNSIGNED_INT: sentinel 0.
            (0, 15, &[0]),
            (1, 19, &[0]),
            (2, 17, &particles),
            (3, 34, &painting),
            (4, 41, &[0; 8]),
            (5, 1, &[9]),
        ]);
        let decoded = values(&bytes);
        assert_eq!(decoded[0], Value::OptionalBlockState(None));
        assert_eq!(decoded[1..3], [const { Value::Other }; 2]);
        assert_eq!(
            values(&frame(&[(11, 15, &[9])])),
            [Value::OptionalBlockState(Some(9))]
        );
        // A painting variant spelled out, its title skipped, and by id.
        assert_eq!(
            decoded[3],
            Value::Painting(Painting::Direct {
                width: 2,
                height: 1,
                asset_id: "a".into()
            })
        );
        assert_eq!(
            values(&frame(&[(8, 34, &[7])])),
            [Value::Painting(Painting::Id(6))]
        );
        // An empty partial profile with no skin patch.
        assert_eq!(decoded[4], Value::Profile(Box::default()));
        assert_eq!(decoded[5], Value::Int(9));
    }

    #[test]
    fn rejects_unknown_serializers_and_truncation() {
        let err = |bytes: &[u8]| read(bytes).map_err(|e| e.what);
        assert_eq!(
            err(&frame(&[(0, 43, &[])])),
            Err("unknown entity data serializer")
        );
        assert_eq!(err(&[0, 3, 0x3F]), Err("truncated"));
        assert_eq!(err(&[0, 1, 1]), Err("truncated"));
    }
}

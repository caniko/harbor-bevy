use core::fmt;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error, IgnoredAny, MapAccess, SeqAccess, Visitor},
    ser::SerializeStruct,
};

use crate::DeterministicRng;

impl Serialize for DeterministicRng {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut fields = serializer.serialize_struct("DeterministicRng", 2)?;
        fields.serialize_field("state", &self.state)?;
        fields.serialize_field("inc", &self.inc)?;
        fields.end()
    }
}

impl<'de> Deserialize<'de> for DeterministicRng {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_struct("DeterministicRng", &["state", "inc"], RngVisitor)
    }
}

struct RngVisitor;

impl<'de> Visitor<'de> for RngVisitor {
    type Value = DeterministicRng;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PCG state and odd increment")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let state = seq
            .next_element()?
            .ok_or_else(|| A::Error::invalid_length(0, &self))?;
        let inc = seq
            .next_element()?
            .ok_or_else(|| A::Error::invalid_length(1, &self))?;
        restore(state, inc)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let (mut state, mut inc) = (None, None);
        while let Some(field) = map.next_key()? {
            match field {
                Field::State if state.is_none() => state = Some(map.next_value()?),
                Field::Increment if inc.is_none() => inc = Some(map.next_value()?),
                Field::State => return Err(A::Error::duplicate_field("state")),
                Field::Increment => return Err(A::Error::duplicate_field("inc")),
                Field::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        restore(
            state.ok_or_else(|| A::Error::missing_field("state"))?,
            inc.ok_or_else(|| A::Error::missing_field("inc"))?,
        )
    }
}

fn restore<E: Error>(state: u64, inc: u64) -> Result<DeterministicRng, E> {
    DeterministicRng::from_state_words([state, inc])
        .ok_or_else(|| E::custom("PCG increment must be odd"))
}

enum Field {
    State,
    Increment,
    Other,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldVisitor;

        impl Visitor<'_> for FieldVisitor {
            type Value = Field;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a PCG state field")
            }

            fn visit_str<E: Error>(self, field: &str) -> Result<Field, E> {
                self.visit_bytes(field.as_bytes())
            }

            fn visit_bytes<E: Error>(self, field: &[u8]) -> Result<Field, E> {
                Ok(match field {
                    b"state" => Field::State,
                    b"inc" => Field::Increment,
                    _ => Field::Other,
                })
            }

            fn visit_u64<E: Error>(self, index: u64) -> Result<Field, E> {
                match index {
                    0 => Ok(Field::State),
                    1 => Ok(Field::Increment),
                    _ => Err(E::invalid_value(
                        serde::de::Unexpected::Unsigned(index),
                        &self,
                    )),
                }
            }
        }

        deserializer.deserialize_identifier(FieldVisitor)
    }
}

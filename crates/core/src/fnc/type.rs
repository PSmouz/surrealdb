use std::ops::Deref;

use crate::ctx::Context;
use crate::dbs::Options;
use crate::doc::CursorDoc;
use crate::err::Error;
use crate::sql::table::Table;
use crate::sql::thing::Thing;
use crate::sql::value::Value;
use crate::sql::{File, FlowResultExt as _, Kind, Strand};
use crate::syn;
use reblessive::tree::Stk;

pub fn array((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_array().map(Value::from)
}

pub fn bool((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_bool().map(Value::from)
}

pub fn file((bucket, key): (String, String)) -> Result<Value, Error> {
	Ok(Value::File(File::new(bucket, key)))
}

pub fn bytes((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_bytes().map(Value::from)
}

pub fn datetime((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_datetime().map(Value::from)
}

pub fn decimal((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_decimal().map(Value::from)
}

pub fn duration((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_duration().map(Value::from)
}

pub async fn field(
	(stk, ctx, opt, doc): (&mut Stk, &Context, Option<&Options>, Option<&CursorDoc>),
	(val,): (String,),
) -> Result<Value, Error> {
	match opt {
		Some(opt) => {
			// Parse the string as an Idiom
			let idi = syn::idiom(&val)?;
			// Return the Idiom or fetch the field
			idi.compute(stk, ctx, opt, doc).await.catch_return()
		}
		_ => Ok(Value::None),
	}
}

pub async fn fields(
	(stk, ctx, opt, doc): (&mut Stk, &Context, Option<&Options>, Option<&CursorDoc>),
	(val,): (Vec<String>,),
) -> Result<Value, Error> {
	match opt {
		Some(opt) => {
			let mut args: Vec<Value> = Vec::with_capacity(val.len());
			for v in val {
				// Parse the string as an Idiom
				let idi = syn::idiom(&v)?;
				// Return the Idiom or fetch the field
				args.push(idi.compute(stk, ctx, opt, doc).await.catch_return()?);
			}
			Ok(args.into())
		}
		_ => Ok(Value::None),
	}
}

pub fn float((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_float().map(Value::from)
}

pub fn geometry((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_geometry().map(Value::from)
}

pub fn int((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_int().map(Value::from)
}

pub fn number((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_number().map(Value::from)
}

pub fn point((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_point().map(Value::from)
}

pub fn range((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_range().map(Value::from)
}

pub fn record((rid, tb): (Value, Option<Value>)) -> Result<Value, Error> {
	match tb {
		Some(Value::Strand(Strand(tb)) | Value::Table(Table(tb))) if tb.is_empty() => {
			Err(Error::TbInvalid {
				value: tb,
			})
		}
		Some(Value::Strand(Strand(tb)) | Value::Table(Table(tb))) => {
			rid.convert_to(&Kind::Record(vec![tb.into()]))
		}
		Some(_) => Err(Error::InvalidArguments {
			name: "type::record".into(),
			message: "The second argument must be a table name or a string.".into(),
		}),
		None => rid.convert_to(&Kind::Record(vec![])),
	}
}

pub fn string((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_strand().map(Value::from)
}

pub fn string_lossy((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_strand_lossy().map(Value::from)
}

pub fn table((val,): (Value,)) -> Result<Value, Error> {
	Ok(Value::Table(Table(match val {
		Value::Thing(t) => t.tb,
		v => v.as_string(),
	})))
}

pub fn thing((arg1, arg2): (Value, Option<Value>)) -> Result<Value, Error> {
	match (arg1, arg2) {
		// Empty table name
		(Value::Strand(arg1), _) if arg1.is_empty() => Err(Error::TbInvalid {
			value: arg1.as_string(),
		}),

		// Empty ID part
		(_, Some(Value::Strand(arg2))) if arg2.is_empty() => Err(Error::IdInvalid {
			value: arg2.as_string(),
		}),

		// Handle second argument
		(arg1, Some(arg2)) => Ok(Value::Thing(Thing {
			tb: arg1.as_string(),
			id: match arg2 {
				Value::Thing(v) => v.id,
				Value::Array(v) => v.into(),
				Value::Object(v) => v.into(),
				Value::Number(v) => v.into(),
				Value::Range(v) => v.deref().to_owned().try_into()?,
				Value::Uuid(u) => u.into(),
				v => v.as_string().into(),
			},
		})),

		// No second argument passed
		(arg1, _) => Ok(match arg1 {
			Value::Thing(v) => Ok(v),
			Value::Strand(v) => Thing::try_from(v.as_str()).map_err(move |_| Error::ConvertTo {
				from: Value::Strand(v),
				into: "record".into(),
			}),
			v => Err(Error::ConvertTo {
				from: v,
				into: "record".into(),
			}),
		}?
		.into()),
	}
}

pub fn uuid((val,): (Value,)) -> Result<Value, Error> {
	val.convert_to_uuid().map(Value::from)
}

pub mod is {
	use crate::err::Error;
	use crate::sql::table::Table;
	use crate::sql::value::Value;
	use crate::sql::Geometry;

	pub fn array((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_array().into())
	}

	pub fn bool((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_bool().into())
	}

	pub fn bytes((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_bytes().into())
	}

	pub fn collection((arg,): (Value,)) -> Result<Value, Error> {
		Ok(matches!(arg, Value::Geometry(Geometry::Collection(_))).into())
	}

	pub fn datetime((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_datetime().into())
	}

	pub fn decimal((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_decimal().into())
	}

	pub fn duration((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_duration().into())
	}

	pub fn float((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_float().into())
	}

	pub fn geometry((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_geometry().into())
	}

	pub fn int((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_int().into())
	}

	pub fn line((arg,): (Value,)) -> Result<Value, Error> {
		Ok(matches!(arg, Value::Geometry(Geometry::Line(_))).into())
	}

	pub fn none((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_none().into())
	}

	pub fn null((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_null().into())
	}

	pub fn multiline((arg,): (Value,)) -> Result<Value, Error> {
		Ok(matches!(arg, Value::Geometry(Geometry::MultiLine(_))).into())
	}

	pub fn multipoint((arg,): (Value,)) -> Result<Value, Error> {
		Ok(matches!(arg, Value::Geometry(Geometry::MultiPoint(_))).into())
	}

	pub fn multipolygon((arg,): (Value,)) -> Result<Value, Error> {
		Ok(matches!(arg, Value::Geometry(Geometry::MultiPolygon(_))).into())
	}

	pub fn number((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_number().into())
	}

	pub fn object((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_object().into())
	}

	pub fn point((arg,): (Value,)) -> Result<Value, Error> {
		Ok(matches!(arg, Value::Geometry(Geometry::Point(_))).into())
	}

	pub fn polygon((arg,): (Value,)) -> Result<Value, Error> {
		Ok(matches!(arg, Value::Geometry(Geometry::Polygon(_))).into())
	}

	pub fn range((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_range().into())
	}

	pub fn record((arg, table): (Value, Option<String>)) -> Result<Value, Error> {
		Ok(match table {
			Some(tb) => arg.is_record_type(&[Table(tb)]).into(),
			None => arg.is_record().into(),
		})
	}

	pub fn string((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_strand().into())
	}

	pub fn uuid((arg,): (Value,)) -> Result<Value, Error> {
		Ok(arg.is_uuid().into())
	}
}

#[cfg(test)]
mod tests {
	use crate::err::Error;
	use crate::sql::value::Value;

	#[test]
	fn is_array() {
		let value = super::is::array((vec!["hello", "world"].into(),)).unwrap();
		assert_eq!(value, Value::Bool(true));

		let value = super::is::array(("test".into(),)).unwrap();
		assert_eq!(value, Value::Bool(false));
	}

	#[test]
	fn no_empty_thing() {
		let value = super::thing(("".into(), None));
		let _expected = Error::TbInvalid {
			value: "".into(),
		};
		if !matches!(value, Err(_expected)) {
			panic!("An empty thing tb part should result in an error");
		}

		let value = super::thing(("table".into(), Some("".into())));
		let _expected = Error::IdInvalid {
			value: "".into(),
		};
		if !matches!(value, Err(_expected)) {
			panic!("An empty thing id part should result in an error");
		}
	}
}

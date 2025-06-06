use crate::ctx::Context;
use crate::dbs::Options;
use crate::err::Error;
use crate::iam::{Action, ResourceKind};
use crate::sql::{Base, Ident, Value};

use revision::revisioned;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display, Formatter};

#[revisioned(revision = 3)]
#[derive(Clone, Debug, Default, Eq, PartialEq, PartialOrd, Serialize, Deserialize, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub struct RemoveNamespaceStatement {
	pub name: Ident,
	#[revision(start = 2)]
	pub if_exists: bool,
	#[revision(start = 3)]
	pub expunge: bool,
}

impl RemoveNamespaceStatement {
	/// Process this type returning a computed simple Value
	pub(crate) async fn compute(&self, ctx: &Context, opt: &Options) -> Result<Value, Error> {
		let future = async {
			// Allowed to run?
			opt.is_allowed(Action::Edit, ResourceKind::Namespace, &Base::Root)?;
			// Get the transaction
			let txn = ctx.tx();
			// Remove the index stores
			#[cfg(not(target_family = "wasm"))]
			ctx.get_index_stores()
				.namespace_removed(ctx.get_index_builder(), &txn, &self.name)
				.await?;
			#[cfg(target_family = "wasm")]
			ctx.get_index_stores().namespace_removed(&txn, &self.name).await?;
			// Get the definition
			let ns = txn.get_ns(&self.name).await?;
			// Delete the definition
			let key = crate::key::root::ns::new(&ns.name);
			match self.expunge {
				true => txn.clr(key).await?,
				false => txn.del(key).await?,
			};
			// Delete the resource data
			let key = crate::key::namespace::all::new(&ns.name);
			match self.expunge {
				true => txn.clrp(key).await?,
				false => txn.delp(key).await?,
			};
			// Clear the cache
			if let Some(cache) = ctx.get_cache() {
				cache.clear();
			}
			// Clear the cache
			txn.clear();
			// Ok all good
			Ok(Value::None)
		}
		.await;
		match future {
			Err(Error::NsNotFound {
				..
			}) if self.if_exists => Ok(Value::None),
			v => v,
		}
	}
}

impl Display for RemoveNamespaceStatement {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		write!(f, "REMOVE NAMESPACE")?;
		if self.if_exists {
			write!(f, " IF EXISTS")?
		}
		write!(f, " {}", self.name)?;
		Ok(())
	}
}

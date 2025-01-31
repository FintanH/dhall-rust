use std::collections::HashMap;
use std::rc::Rc;

use dhall_syntax::{Label, V};

use crate::core::value::Value;
use crate::core::valuef::ValueF;
use crate::core::var::{AlphaVar, Shift, Subst};
use crate::error::TypeError;

#[derive(Debug, Clone)]
enum CtxItem {
    Kept(AlphaVar, Value),
    Replaced(Value),
}

#[derive(Debug, Clone)]
pub(crate) struct TypecheckContext(Rc<Vec<(Label, CtxItem)>>);

impl TypecheckContext {
    pub fn new() -> Self {
        TypecheckContext(Rc::new(Vec::new()))
    }
    pub fn insert_type(&self, x: &Label, t: Value) -> Self {
        let mut vec = self.0.as_ref().clone();
        vec.push((x.clone(), CtxItem::Kept(x.into(), t.under_binder(x))));
        TypecheckContext(Rc::new(vec))
    }
    pub fn insert_value(&self, x: &Label, e: Value) -> Result<Self, TypeError> {
        let mut vec = self.0.as_ref().clone();
        vec.push((x.clone(), CtxItem::Replaced(e)));
        Ok(TypecheckContext(Rc::new(vec)))
    }
    pub fn lookup(&self, var: &V<Label>) -> Option<Value> {
        let mut var = var.clone();
        let mut shift_map: HashMap<Label, _> = HashMap::new();
        for (l, i) in self.0.iter().rev() {
            match var.over_binder(l) {
                None => {
                    let i = i.under_multiple_binders(&shift_map);
                    return Some(match i {
                        CtxItem::Kept(newvar, t) => {
                            Value::from_valuef_and_type(ValueF::Var(newvar), t)
                        }
                        CtxItem::Replaced(v) => v,
                    });
                }
                Some(newvar) => var = newvar,
            };
            if let CtxItem::Kept(_, _) = i {
                *shift_map.entry(l.clone()).or_insert(0) += 1;
            }
        }
        // Unbound variable
        None
    }
    /// Given a var that makes sense in the current context, map the given function in such a way
    /// that the passed variable always makes sense in the context of the passed item.
    /// Once we pass the variable definition, the variable doesn't make sense anymore so we just
    /// copy the remaining items.
    fn do_with_var<E>(
        &self,
        var: &AlphaVar,
        mut f: impl FnMut(&AlphaVar, &CtxItem) -> Result<CtxItem, E>,
    ) -> Result<Self, E> {
        let mut vec = Vec::new();
        vec.reserve(self.0.len());
        let mut var = var.clone();
        let mut iter = self.0.iter().rev();
        for (l, i) in iter.by_ref() {
            vec.push((l.clone(), f(&var, i)?));
            if let CtxItem::Kept(_, _) = i {
                match var.over_binder(l) {
                    None => break,
                    Some(newvar) => var = newvar,
                };
            }
        }
        for (l, i) in iter {
            vec.push((l.clone(), (*i).clone()));
        }
        vec.reverse();
        Ok(TypecheckContext(Rc::new(vec)))
    }
    fn shift(&self, delta: isize, var: &AlphaVar) -> Option<Self> {
        if delta < 0 {
            Some(self.do_with_var(var, |var, i| Ok(i.shift(delta, &var)?))?)
        } else {
            Some(TypecheckContext(Rc::new(
                self.0
                    .iter()
                    .map(|(l, i)| Ok((l.clone(), i.shift(delta, &var)?)))
                    .collect::<Result<_, _>>()?,
            )))
        }
    }
    fn subst_shift(&self, var: &AlphaVar, val: &Value) -> Self {
        self.do_with_var(var, |var, i| Ok::<_, !>(i.subst_shift(&var, val)))
            .unwrap()
    }
}

impl Shift for CtxItem {
    fn shift(&self, delta: isize, var: &AlphaVar) -> Option<Self> {
        Some(match self {
            CtxItem::Kept(v, t) => {
                CtxItem::Kept(v.shift(delta, var)?, t.shift(delta, var)?)
            }
            CtxItem::Replaced(e) => CtxItem::Replaced(e.shift(delta, var)?),
        })
    }
}

impl Shift for TypecheckContext {
    fn shift(&self, delta: isize, var: &AlphaVar) -> Option<Self> {
        self.shift(delta, var)
    }
}

impl Subst<Value> for CtxItem {
    fn subst_shift(&self, var: &AlphaVar, val: &Value) -> Self {
        match self {
            CtxItem::Replaced(e) => CtxItem::Replaced(e.subst_shift(var, val)),
            CtxItem::Kept(v, t) => match v.shift(-1, var) {
                None => CtxItem::Replaced(val.clone()),
                Some(newvar) => CtxItem::Kept(newvar, t.subst_shift(var, val)),
            },
        }
    }
}

impl Subst<Value> for TypecheckContext {
    fn subst_shift(&self, var: &AlphaVar, val: &Value) -> Self {
        self.subst_shift(var, val)
    }
}

/// Don't count contexts when comparing stuff.
/// This is dirty but needed.
impl PartialEq for TypecheckContext {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for TypecheckContext {}

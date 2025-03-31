//! Koto bindings to Fidget
use std::ops::{Add, Div, Mul, Rem, Sub};
use std::sync::{Arc, Mutex};

use crate::{context::Tree, Error};
use koto::{derive::*, prelude::*, runtime};

macro_rules! define_binary_op_fns {
    ($koto_name:ident, $tree_name:ident) => {
        fn $koto_name(&self, rhs: &KValue) -> runtime::Result<KValue> {
            let lhs = self.0.clone();
            match rhs {
                KValue::Number(num) => {
                    let tree = lhs.$tree_name(Tree::constant(f64::from(num)));
                    Ok(KValue::Object(Self(tree).into()))
                }
                KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                    let koto_tree = obj.cast::<KotoTree>();
                    let tree = koto_tree.unwrap().to_owned().0;
                    let result = lhs.$tree_name(tree);
                    Ok(KValue::Object(Self(result).into()))
                }
                unexpected => {
                    let err_msg = format!(
                        "invalid type for {}(Tree, rhs)",
                        stringify!($tree_name)
                    );
                    unexpected_type(&err_msg, unexpected)
                }
            }
        }
    };
}

// macro_rules! register_fn {
//     ($ctx:item, $name:ident) => {
//         let args = $ctx.args();
//         if args.len() != 1 {
//             return unexpected_args("1 argument: tree or number", args);
//         }
//         match &args[0] {
//             KValue::Object(obj) if obj.is_a::<KotoTree>() => {
//                 let tree = obj.cast::<KotoTree>()?.to_owned().0;
//                 let result = tree.$name();
//                 Ok(KotoTree::make_koto_object(result).into())
//             }
//             // TODO: check and handle number
//             unexpected => unexpected_type("invalid type", unexpected),
//         }
//     };
// }

#[derive(Clone, KotoCopy, KotoType)]
#[koto(type_name = "Tree")]
struct KotoTree(Tree);

impl KotoObject for KotoTree {
    define_binary_op_fns!(add, add);
    define_binary_op_fns!(subtract, sub);
    define_binary_op_fns!(multiply, mul);
    define_binary_op_fns!(divide, div);
    define_binary_op_fns!(remainder, modulo);
}

#[koto_impl]
impl KotoTree {
    fn make_koto_object(tree: Tree) -> KObject {
        let koto_tree = Self(tree.into());
        KObject::from(koto_tree)
    }

    fn x() -> KObject {
        let koto_tree = Self(Tree::x().into());
        KObject::from(koto_tree)
    }

    fn y() -> KObject {
        let koto_tree = Self(Tree::y().into());
        KObject::from(koto_tree)
    }

    fn z() -> KObject {
        let koto_tree = Self(Tree::z().into());
        KObject::from(koto_tree)
    }
}

/// Engine for evaluating a Koto script with Fidget-specific bindings
pub struct Engine {
    engine: Koto,
    context: Arc<Mutex<ScriptContext>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// Constructs a script evaluation engine with Fidget bindings
    ///
    /// The context includes a variety of functions that operate on [`Tree`]
    /// handles.
    ///
    /// In addition, it includes everything in [`core.koto`](crate::koto::core),
    /// which is effectively our standard library.
    pub fn new() -> Self {
        let engine = Koto::default();

        engine.prelude().insert("x", KotoTree::x());
        engine.prelude().insert("y", KotoTree::y());
        engine.prelude().insert("z", KotoTree::z());

        engine.prelude().add_fn("axes", move |_ctx| {
            let (x, y, z) = Tree::axes();
            Ok(KValue::Tuple(KTuple::from(vec![
                KValue::Object(KotoTree::make_koto_object(x).into()),
                KValue::Object(KotoTree::make_koto_object(y).into()),
                KValue::Object(KotoTree::make_koto_object(z).into()),
            ])))
        });

        // CAN BE REMOVED, we use remap in KotoTree
        engine.prelude()
            .add_fn("remap_xyz", move |ctx| {
                let args = ctx.args();
                if args.len() != 4 {
                    return unexpected_args("4 arguments: shape, x, y, z", args);
                }
                match args {
                    [KValue::Object(obj),
                        KValue::Object(obj_x),
                        KValue::Object(obj_y),
                        KValue::Object(obj_z)] => {
                        if obj.is_a::<KotoTree>() && obj_x.is_a::<KotoTree>() && obj_y.is_a::<KotoTree>() && obj_z.is_a::<KotoTree>() {
                            let tree = obj.cast::<KotoTree>()?.to_owned().0;
                            let x = obj_x.cast::<KotoTree>()?.to_owned().0;
                            let y = obj_y.cast::<KotoTree>()?.to_owned().0;
                            let z = obj_z.cast::<KotoTree>()?.to_owned().0;
                            let result = tree.remap_xyz(x, y, z);
                            Ok(KotoTree::make_koto_object(result).into())
                        } else {
                            unexpected_args("invalid type", args)
                        }
                    }
                    _ => unexpected_args("invalid type", args)
                }
            });

        macro_rules! add_unary_fn {
            ($text:literal, $name:ident) => {
                // $engine.register_fn($op, $name::node);
                engine.prelude().add_fn($text, move |ctx| {
                    let args = ctx.args();
                    if args.len() != 1 {
                        return unexpected_args(
                            "1 argument: tree or number",
                            args,
                        );
                    }
                    match &args[0] {
                        KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                            let tree = obj.cast::<KotoTree>()?.to_owned().0;
                            let result = tree.$name();
                            Ok(KotoTree::make_koto_object(result).into())
                        }
                        // TODO: check and handle number
                        unexpected => {
                            unexpected_type("invalid type", unexpected)
                        }
                    }
                });
            };
        }

        // register_binary_fns!("min", min, engine);
        // register_binary_fns!("max", max, engine);
        // register_binary_fns!("compare", compare, engine);
        // register_binary_fns!("and", and, engine);
        // register_binary_fns!("or", or, engine);
        // register_binary_fns!("atan2", atan2, engine);

        add_unary_fn!("abs", abs);
        add_unary_fn!("sqrt", sqrt);
        add_unary_fn!("square", square);
        add_unary_fn!("sin", sin);
        add_unary_fn!("cos", cos);
        add_unary_fn!("tan", tan);
        add_unary_fn!("asin", asin);
        add_unary_fn!("acos", acos);
        add_unary_fn!("atan", atan);
        add_unary_fn!("exp", exp);
        add_unary_fn!("ln", ln);
        add_unary_fn!("not", not);
        add_unary_fn!("ceil", ceil);
        add_unary_fn!("floor", floor);
        add_unary_fn!("round", round);

        // register_unary_fns!("-", neg, engine);

        let context = Arc::new(Mutex::new(ScriptContext::new()));

        Self { engine, context }
    }

    /// Executes a full script
    pub fn run(&mut self, script: &str) -> Result<ScriptContext, Error> {
        self.context.lock().unwrap().clear();

        match self.engine.compile_and_run(script) {
            Ok(KValue::List(list)) => {
                for el in list.data().iter() {
                    match el {
                        KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                            let koto_tree = obj.cast::<KotoTree>();
                            let tree = koto_tree.unwrap().to_owned().0;
                            self.context.lock().unwrap().shapes.push(
                                DrawShape {
                                    tree,
                                    color_rgb: [u8::MAX; 3],
                                },
                            )
                        }
                        // TODO: if tuple containing color then do as in rhai viewer
                        _ => (),
                    }
                }
            }
            // TODO: check for single shape (KotoTree object)
            Ok(_) => println!("No shapes returned"),
            Err(err) => println!("compile error:{}", err),
        }

        // Steal the ScriptContext's contents
        let mut lock = self.context.lock().unwrap();
        Ok(std::mem::take(&mut lock))
    }

    /// Evaluates a single expression, in terms of `x`, `y`, and `z`
    pub fn eval(&mut self, script: &str) -> Result<Tree, Error> {
        match self.engine.compile_and_run(script) {
            Ok(KValue::Object(obj)) if obj.is_a::<KotoTree>() => {
                let koto_tree = obj.cast::<KotoTree>();
                let tree = koto_tree.unwrap().to_owned().0;
                Ok(tree)
            }
            Ok(_) => Err(Error::BadNode),
            Err(_) => Err(Error::BadNode),
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////

/// Shape to render
///
/// Populated by calls to `draw(...)` or `draw_rgb(...)` in a Koto script
pub struct DrawShape {
    /// Tree to render
    pub tree: Tree,
    /// Color to use when drawing the shape
    pub color_rgb: [u8; 3],
}

/// Context for shape evaluation
///
/// This object stores a set of shapes, which is populated by calls to `draw` or
/// `draw_rgb` during script evaluation.
pub struct ScriptContext {
    /// List of shapes populated since the last call to [`clear`](Self::clear)
    pub shapes: Vec<DrawShape>,
}

impl Default for ScriptContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptContext {
    /// Builds a new empty script context
    pub fn new() -> Self {
        Self { shapes: vec![] }
    }
    /// Resets the script context
    pub fn clear(&mut self) {
        self.shapes.clear();
    }
}

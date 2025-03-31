//! Koto bindings to Fidget
use std::ops::{Add, Div, Mul, Sub};
use std::sync::{Arc, Mutex};

use crate::{context::Tree, Error};
use koto::{derive::*, prelude::*, runtime};

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

        // CAN BE REMOVED, we just export a shape
        // engine.prelude()
        //     .add_fn("draw", move |ctx| {
        //             let args = ctx.args();
        //             if args.len() != 1 {
        //                 return unexpected_args("1 argument: shape", args);
        //             }
        //             match &args[0] {
        //                 KValue::Object(obj) if obj.is_a::<KotoTree>() => {
        //                     // TODO
        //                     // let ctx = ctx.tag().unwrap().clone_cast::<Arc<Mutex<ScriptContext>>>();
        //                     // ctx.lock().unwrap().shapes.push(DrawShape {
        //                     //     tree,
        //                     //     color_rgb: [u8::MAX; 3],
        //                     // });
        //                     Ok(())
        //                 }
        //                 unexpected => unexpected_args("wrong argument type", unexpected),
        //             }
        //         });

        // CAN BE REMOVED, we just export a shape as part of tupe or list which also contains rgb
        // engine.prelude()
        //     .add_fn("draw_rgb", move |ctx| {
        //         let args = ctx.args();
        //         if args.len() != 4 {
        //             return unexpected_args("4 arguments: shape, r, g, b", args);
        //         }
        //         // TODO
        //         // let ctx = ctx.tag().unwrap().clone_cast::<Arc<Mutex<ScriptContext>>>();
        //         // let f = |a| {
        //         //     if a < 0.0 {
        //         //         0
        //         //     } else if a > 1.0 {
        //         //         255
        //         //     } else {
        //         //         (a * 255.0) as u8
        //         //     }
        //         // };
        //         // ctx.lock().unwrap().shapes.push(DrawShape {
        //         //     tree,
        //         //     color_rgb: [f(r), f(g), f(b)],
        //         // });
        //         Ok(())
        //     });

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
                            unexpected_args("wrong argument type", args)
                        }
                    }
                    unexpected => unexpected_args("wrong argument type", unexpected),
                }
            });

        engine.prelude().add_fn("sqrt", move |ctx| {
            let args = ctx.args();
            if args.len() != 1 {
                return unexpected_args("1 argument: tree or number", args);
            }
            match &args[0] {
                KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                    let tree = obj.cast::<KotoTree>()?.to_owned().0;
                    let result = tree.sqrt();
                    Ok(KotoTree::make_koto_object(result).into())
                }
                // TODO: check and handle number
                _ => unexpected_args("wrong argument type", args),
            }
        });

        engine.prelude().add_fn("square", move |ctx| {
            let args = ctx.args();
            if args.len() != 1 {
                return unexpected_args("1 argument: tree or number", args);
            }
            match &args[0] {
                KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                    let tree = obj.cast::<KotoTree>()?.to_owned().0;
                    let result = tree.square();
                    Ok(KotoTree::make_koto_object(result).into())
                }
                // TODO: check and handle number
                _ => unexpected_args("wrong argument type", args),
            }
        });

        let context = Arc::new(Mutex::new(ScriptContext::new()));

        Self { engine, context }
    }

    /// Executes a full script
    pub fn run(&mut self, script: &str) -> Result<ScriptContext, Error> {
        self.context.lock().unwrap().clear();

        match self.engine.compile_and_run(script) {
            Ok(_) => {
                // alternative for draw and draw_rgb
                for (i, (key, value)) in
                    self.engine.exports().data().iter().enumerate()
                {
                    match value {
                        KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                            println!(
                                "exported tree - index: {} | name: {}",
                                i, key
                            );
                            let koto_tree = obj.cast::<KotoTree>();
                            let tree = koto_tree.unwrap().to_owned().0;
                            self.context.lock().unwrap().shapes.push(
                                DrawShape {
                                    tree,
                                    color_rgb: [u8::MAX; 3],
                                },
                            )
                        }
                        // TODO: check for draw_rgb equivalent
                        other => {
                            println!("exported unhandled other: {:#?}", other);
                            // TODO: create warning ???
                        }
                    }
                }
            }
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

// new stuff
#[derive(Clone, KotoCopy, KotoType)]
#[koto(type_name = "Tree")]
struct KotoTree(Tree);

impl KotoObject for KotoTree {
    fn add(&self, rhs: &KValue) -> runtime::Result<KValue> {
        let lhs = self.0.clone();
        match rhs {
            KValue::Number(num) => {
                let tree = lhs.add(Tree::constant(f64::from(num)));
                Ok(KValue::Object(Self(tree).into()))
            }
            KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                let koto_tree = obj.cast::<KotoTree>();
                let tree = koto_tree.unwrap().to_owned().0;
                let result = lhs.add(tree);
                Ok(KValue::Object(Self(result).into()))
            }
            // Example from Ohm
            // (Self::Constant(node), KValue::Number(num)) => {
            //     Ok(KValue::Object(constant(node.value + f32::from(num)).into()))
            // }
            // (_, KValue::Number(num)) => Ok(KValue::Object(
            //     mix(self.clone(), constant((num).into())).into(),
            // )),
            // (_, KValue::Object(obj)) => Ok(KValue::Object(
            //     mix(self.clone(), obj.cast::<NodeKind>()?.clone()).into(),
            // )),
            // Rhai version for expresion: a op b (a is tree, b can be anything)
            // let b = if let Some(v) = b.clone().try_cast::<f64>() {
            //     Tree::constant(v)
            // } else if let Some(v) = b.clone().try_cast::<i64>() {
            //     Tree::constant(v as f64)
            // } else if let Some(t) = b.clone().try_cast::<Tree>() {
            //     t
            // } else {
            //     let e = format!(
            //         "invalid type for {}(Tree, rhs): {}",
            //         stringify!($name),
            //         b.type_name()
            //     );
            //     return Err(e.into());
            // };
            // Ok(a.$name(b))
            _ => panic!("invalid add operator"),
        }
    }

    fn subtract(&self, rhs: &KValue) -> runtime::Result<KValue> {
        let lhs = self.0.clone();
        match rhs {
            KValue::Number(num) => {
                let tree = lhs.sub(Tree::constant(f64::from(num)));
                Ok(KValue::Object(Self(tree).into()))
            }
            KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                let koto_tree = obj.cast::<KotoTree>();
                let tree = koto_tree.unwrap().to_owned().0;
                let result = lhs.sub(tree);
                Ok(KValue::Object(Self(result).into()))
            }
            _ => panic!("invalid subtract operator"),
        }
    }

    fn multiply(&self, rhs: &KValue) -> runtime::Result<KValue> {
        let lhs = self.0.clone();
        match rhs {
            KValue::Number(num) => {
                let tree = lhs.mul(Tree::constant(f64::from(num)));
                Ok(KValue::Object(Self(tree).into()))
            }
            KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                let koto_tree = obj.cast::<KotoTree>();
                let tree = koto_tree.unwrap().to_owned().0;
                let result = lhs.mul(tree);
                Ok(KValue::Object(Self(result).into()))
            }
            _ => panic!("invalid multiply operator"),
        }
    }

    fn divide(&self, rhs: &KValue) -> runtime::Result<KValue> {
        let lhs = self.0.clone();
        match rhs {
            KValue::Number(num) => {
                let tree = lhs.div(Tree::constant(f64::from(num)));
                Ok(KValue::Object(Self(tree).into()))
            }
            KValue::Object(obj) if obj.is_a::<KotoTree>() => {
                let koto_tree = obj.cast::<KotoTree>();
                let tree = koto_tree.unwrap().to_owned().0;
                let result = lhs.div(tree);
                Ok(KValue::Object(Self(result).into()))
            }
            _ => panic!("invalid divide operator"),
        }
    }
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

    // A function that returns the object instance as the result
    #[koto_method]
    fn draw(ctx: MethodContext<Self>) -> runtime::Result<KValue> {
        // TODO: check args for rgb
        let _koto_tree = ctx.instance();
        // let mut _kmap = ctx.vm.exports_mut();
        // TODO: create empty exported list "draw"
        // if let Some(KValue::List(list)) = kmap.get("draw") {
        //     list.clone().data_mut().push(koto_tree);  // what type to push ???
        // }
        ctx.instance_result()
    }
}

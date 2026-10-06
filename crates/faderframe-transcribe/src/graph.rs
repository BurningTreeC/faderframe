//! A small evaluator for the operator subset (ONNX opset 15) of the
//! converted basic-pitch graph (`scripts/basic_pitch_model.py`): tensors
//! of f32, i64 and bool, numpy broadcasting, and the operators the graph
//! uses — Reshape, Unsqueeze, Slice, Pad (constant, reflect), Transpose,
//! Concat, Conv (2-D), Neg, Mul, Add, Sub, Div, Cast, Relu, Sigmoid, Sqrt,
//! Log, ReduceSum/Min/Max, Shape, Equal, Where. Nodes run in file order (a
//! topological order); values are dropped after their last use.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("not a model file: {0}")]
    Format(String),
    #[error("{op}: {msg}")]
    Op { op: String, msg: String },
    #[error("missing value {0}")]
    Missing(String),
}

fn err(op: &str, msg: impl Into<String>) -> GraphError {
    GraphError::Op {
        op: op.into(),
        msg: msg.into(),
    }
}

/// A tensor: shape and row-major data.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor<T> {
    pub shape: Vec<usize>,
    pub data: Vec<T>,
}

impl<T: Copy> Tensor<T> {
    pub fn new(shape: Vec<usize>, data: Vec<T>) -> Self {
        debug_assert_eq!(shape.iter().product::<usize>(), data.len());
        Self { shape, data }
    }

    fn reshaped(self, shape: Vec<usize>) -> Self {
        Self {
            shape,
            data: self.data,
        }
    }
}

/// A value flowing through the graph.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    F(Tensor<f32>),
    I(Tensor<i64>),
    B(Tensor<bool>),
}

impl Value {
    fn shape(&self) -> &[usize] {
        match self {
            Value::F(t) => &t.shape,
            Value::I(t) => &t.shape,
            Value::B(t) => &t.shape,
        }
    }

    fn ints(&self, op: &str) -> Result<Vec<i64>, GraphError> {
        match self {
            Value::I(t) => Ok(t.data.clone()),
            Value::F(t) => Ok(t.data.iter().map(|v| *v as i64).collect()),
            Value::B(_) => Err(err(op, "integers expected")),
        }
    }

    fn floats(&self, op: &str) -> Result<&Tensor<f32>, GraphError> {
        match self {
            Value::F(t) => Ok(t),
            _ => Err(err(op, "floats expected")),
        }
    }

    /// The same data in a new shape (any type).
    fn with_shape(self, shape: Vec<usize>) -> Value {
        match self {
            Value::F(t) => Value::F(t.reshaped(shape)),
            Value::I(t) => Value::I(t.reshaped(shape)),
            Value::B(t) => Value::B(t.reshaped(shape)),
        }
    }

    /// Gather elements by flat index (any type).
    fn gather(&self, shape: Vec<usize>, index: &[usize]) -> Value {
        match self {
            Value::F(t) => Value::F(Tensor::new(
                shape,
                index.iter().map(|i| t.data[*i]).collect(),
            )),
            Value::I(t) => Value::I(Tensor::new(
                shape,
                index.iter().map(|i| t.data[*i]).collect(),
            )),
            Value::B(t) => Value::B(Tensor::new(
                shape,
                index.iter().map(|i| t.data[*i]).collect(),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TensorInfo {
    name: String,
    dtype: String,
    shape: Vec<usize>,
    offset: usize,
    len: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum Attr {
    Int(i64),
    Float(f64),
    Ints(Vec<i64>),
    // Accepted, not used by the graph's operators.
    #[allow(dead_code)]
    Floats(Vec<f64>),
    Text(String),
}

#[derive(Debug, Deserialize)]
struct Node {
    op: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    #[serde(default)]
    attrs: HashMap<String, Attr>,
}

impl Node {
    fn int(&self, name: &str, default: i64) -> i64 {
        match self.attrs.get(name) {
            Some(Attr::Int(v)) => *v,
            Some(Attr::Float(v)) => *v as i64,
            _ => default,
        }
    }

    fn ints(&self, name: &str) -> Option<Vec<i64>> {
        match self.attrs.get(name) {
            Some(Attr::Ints(v)) => Some(v.clone()),
            Some(Attr::Int(v)) => Some(vec![*v]),
            _ => None,
        }
    }

    fn text(&self, name: &str) -> Option<&str> {
        match self.attrs.get(name) {
            Some(Attr::Text(s)) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Header {
    inputs: Vec<String>,
    outputs: Vec<String>,
    tensors: Vec<TensorInfo>,
    nodes: Vec<Node>,
}

/// A loaded graph.
#[derive(Debug)]
pub struct Graph {
    inputs: Vec<String>,
    outputs: Vec<String>,
    constants: HashMap<String, Value>,
    nodes: Vec<Node>,
    /// The index of the last node reading each value.
    last_use: HashMap<String, usize>,
}

impl Graph {
    /// Parse a converted model (`FFNN1`, a JSON header, the data).
    pub fn parse(bytes: &[u8]) -> Result<Self, GraphError> {
        let fmt = |m: &str| GraphError::Format(m.into());
        let rest = bytes.strip_prefix(b"FFNN1\n").ok_or_else(|| fmt("magic"))?;
        let len = rest
            .get(..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| fmt("header length"))?;
        let head = rest.get(4..4 + len).ok_or_else(|| fmt("header"))?;
        let data = &rest[4 + len..];
        let header: Header =
            serde_json::from_slice(head).map_err(|e| GraphError::Format(e.to_string()))?;
        let mut constants = HashMap::new();
        for t in &header.tensors {
            let raw = data
                .get(t.offset..t.offset + t.len)
                .ok_or_else(|| fmt("tensor data"))?;
            let value = match t.dtype.as_str() {
                "f32" => Value::F(Tensor::new(
                    t.shape.clone(),
                    raw.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| f32::from_le_bytes(*c))
                        .collect(),
                )),
                "i64" => Value::I(Tensor::new(
                    t.shape.clone(),
                    raw.as_chunks::<8>()
                        .0
                        .iter()
                        .map(|c| i64::from_le_bytes(*c))
                        .collect(),
                )),
                other => return Err(fmt(&format!("dtype {other}"))),
            };
            constants.insert(t.name.clone(), value);
        }
        let mut last_use = HashMap::new();
        for (i, n) in header.nodes.iter().enumerate() {
            for input in &n.inputs {
                last_use.insert(input.clone(), i);
            }
        }
        for o in &header.outputs {
            last_use.insert(o.clone(), usize::MAX);
        }
        Ok(Self {
            inputs: header.inputs,
            outputs: header.outputs,
            constants,
            nodes: header.nodes,
            last_use,
        })
    }

    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Run the graph on its (first) input; returns the outputs by name.
    pub fn run(&self, input: Tensor<f32>) -> Result<HashMap<String, Value>, GraphError> {
        let mut env: HashMap<&str, Value> = HashMap::new();
        let name = self
            .inputs
            .first()
            .ok_or_else(|| GraphError::Format("no input".into()))?;
        env.insert(name, Value::F(input));
        for (i, node) in self.nodes.iter().enumerate() {
            let args: Vec<Option<&Value>> = node
                .inputs
                .iter()
                .map(|n| {
                    if n.is_empty() {
                        None
                    } else {
                        env.get(n.as_str()).or_else(|| self.constants.get(n))
                    }
                })
                .collect();
            for (n, a) in node.inputs.iter().zip(&args) {
                if !n.is_empty() && a.is_none() {
                    return Err(GraphError::Missing(n.clone()));
                }
            }
            let out = eval(node, &args)?;
            for name in &node.inputs {
                if self.last_use.get(name) == Some(&i) {
                    env.remove(name.as_str());
                }
            }
            if let Some(o) = node.outputs.first() {
                env.insert(o, out);
            }
        }
        self.outputs
            .iter()
            .map(|o| {
                env.remove(o.as_str())
                    .map(|v| (o.clone(), v))
                    .ok_or_else(|| GraphError::Missing(o.clone()))
            })
            .collect()
    }
}

fn arg<'a>(node: &Node, args: &[Option<&'a Value>], i: usize) -> Result<&'a Value, GraphError> {
    args.get(i)
        .copied()
        .flatten()
        .ok_or_else(|| err(&node.op, format!("input {i} missing")))
}

fn strides(shape: &[usize]) -> Vec<usize> {
    let mut s = vec![1; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        s[i] = s[i + 1] * shape[i + 1];
    }
    s
}

/// An axis given relative to `rank` (negative from the end).
fn axis(a: i64, rank: usize) -> usize {
    if a < 0 {
        (rank as i64 + a).max(0) as usize
    } else {
        a as usize
    }
}

/// The broadcast shape of two shapes, numpy style.
fn broadcast(a: &[usize], b: &[usize], op: &str) -> Result<Vec<usize>, GraphError> {
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| {
            let x = if i + a.len() >= n {
                a[i + a.len() - n]
            } else {
                1
            };
            let y = if i + b.len() >= n {
                b[i + b.len() - n]
            } else {
                1
            };
            match (x, y) {
                _ if x == y => Ok(x),
                (1, _) => Ok(y),
                (_, 1) => Ok(x),
                _ => Err(err(op, format!("shapes {a:?} and {b:?} do not broadcast"))),
            }
        })
        .collect()
}

/// Flat indices into a tensor of `shape` for each element of `out`
/// (broadcast).
fn broadcast_index(shape: &[usize], out: &[usize]) -> Vec<usize> {
    let n = out.len();
    let st = strides(shape);
    // Each output axis's stride in the input (0 where broadcast).
    let s: Vec<usize> = (0..n)
        .map(|i| {
            if i + shape.len() >= n {
                let j = i + shape.len() - n;
                if shape[j] == 1 { 0 } else { st[j] }
            } else {
                0
            }
        })
        .collect();
    let total: usize = out.iter().product();
    let mut idx = Vec::with_capacity(total);
    let mut counter = vec![0usize; n];
    let mut flat = 0usize;
    for _ in 0..total {
        idx.push(flat);
        for d in (0..n).rev() {
            counter[d] += 1;
            flat += s[d];
            if counter[d] < out[d] {
                break;
            }
            flat -= s[d] * counter[d];
            counter[d] = 0;
        }
    }
    idx
}

fn binary_f(
    node: &Node,
    a: &Tensor<f32>,
    b: &Tensor<f32>,
    f: impl Fn(f32, f32) -> f32,
) -> Result<Tensor<f32>, GraphError> {
    if a.shape == b.shape {
        return Ok(Tensor::new(
            a.shape.clone(),
            a.data.iter().zip(&b.data).map(|(x, y)| f(*x, *y)).collect(),
        ));
    }
    if b.data.len() == 1 {
        let y = b.data[0];
        let shape = broadcast(&a.shape, &b.shape, &node.op)?;
        if shape == a.shape {
            return Ok(Tensor::new(
                shape,
                a.data.iter().map(|x| f(*x, y)).collect(),
            ));
        }
    }
    let shape = broadcast(&a.shape, &b.shape, &node.op)?;
    let ia = broadcast_index(&a.shape, &shape);
    let ib = broadcast_index(&b.shape, &shape);
    Ok(Tensor::new(
        shape,
        ia.iter()
            .zip(&ib)
            .map(|(i, j)| f(a.data[*i], b.data[*j]))
            .collect(),
    ))
}

fn eval(node: &Node, args: &[Option<&Value>]) -> Result<Value, GraphError> {
    let op = node.op.as_str();
    match op {
        "Reshape" => {
            let x = arg(node, args, 0)?;
            let want = arg(node, args, 1)?.ints(op)?;
            let total: usize = x.shape().iter().product();
            let mut shape: Vec<usize> = Vec::with_capacity(want.len());
            let mut infer = None;
            for (i, d) in want.iter().enumerate() {
                match *d {
                    0 => shape.push(*x.shape().get(i).ok_or_else(|| err(op, "0 beyond rank"))?),
                    -1 => {
                        infer = Some(i);
                        shape.push(1);
                    }
                    d if d > 0 => shape.push(d as usize),
                    _ => return Err(err(op, "bad dimension")),
                }
            }
            if let Some(i) = infer {
                let known: usize = shape.iter().product();
                shape[i] = total / known.max(1);
            }
            if shape.iter().product::<usize>() != total {
                return Err(err(op, format!("{:?} to {want:?}", x.shape())));
            }
            Ok(x.clone().with_shape(shape))
        }
        "Unsqueeze" => {
            let x = arg(node, args, 0)?;
            let axes = match args.get(1).copied().flatten() {
                Some(a) => a.ints(op)?,
                None => node.ints("axes").unwrap_or_default(),
            };
            let rank = x.shape().len() + axes.len();
            let mut ax: Vec<usize> = axes.iter().map(|a| axis(*a, rank)).collect();
            ax.sort_unstable();
            let mut shape = x.shape().to_vec();
            for a in ax {
                shape.insert(a.min(shape.len()), 1);
            }
            Ok(x.clone().with_shape(shape))
        }
        "Squeeze" => {
            let x = arg(node, args, 0)?;
            let axes = match args.get(1).copied().flatten() {
                Some(a) => Some(a.ints(op)?),
                None => node.ints("axes"),
            };
            let rank = x.shape().len();
            let drop: Vec<usize> = match axes {
                Some(a) => a.iter().map(|v| axis(*v, rank)).collect(),
                None => (0..rank).filter(|i| x.shape()[*i] == 1).collect(),
            };
            let shape = x
                .shape()
                .iter()
                .enumerate()
                .filter(|(i, _)| !drop.contains(i))
                .map(|(_, d)| *d)
                .collect();
            Ok(x.clone().with_shape(shape))
        }
        "Transpose" => {
            let x = arg(node, args, 0)?;
            let rank = x.shape().len();
            let perm: Vec<usize> = node
                .ints("perm")
                .map(|p| p.iter().map(|v| *v as usize).collect())
                .unwrap_or_else(|| (0..rank).rev().collect());
            let shape: Vec<usize> = perm.iter().map(|p| x.shape()[*p]).collect();
            let st = strides(x.shape());
            // Output element k's input index.
            let total: usize = shape.iter().product();
            let mut index = Vec::with_capacity(total);
            let mut counter = vec![0usize; rank];
            let s: Vec<usize> = perm.iter().map(|p| st[*p]).collect();
            let mut flat = 0usize;
            for _ in 0..total {
                index.push(flat);
                for d in (0..rank).rev() {
                    counter[d] += 1;
                    flat += s[d];
                    if counter[d] < shape[d] {
                        break;
                    }
                    flat -= s[d] * counter[d];
                    counter[d] = 0;
                }
            }
            Ok(x.gather(shape, &index))
        }
        "Concat" => {
            let parts: Vec<&Value> = args.iter().copied().flatten().collect();
            let first = parts.first().ok_or_else(|| err(op, "nothing to join"))?;
            let rank = first.shape().len();
            let ax = axis(node.int("axis", 0), rank);
            let mut shape = first.shape().to_vec();
            shape[ax] = parts.iter().map(|p| p.shape()[ax]).sum();
            let outer: usize = shape[..ax].iter().product();
            let inner: usize = shape[ax + 1..].iter().product();
            macro_rules! join {
                ($variant:ident) => {{
                    let mut data = Vec::with_capacity(shape.iter().product());
                    for o in 0..outer {
                        for p in &parts {
                            let Value::$variant(t) = p else {
                                return Err(err(op, "mixed types"));
                            };
                            let len = t.shape[ax] * inner;
                            data.extend_from_slice(&t.data[o * len..(o + 1) * len]);
                        }
                    }
                    Value::$variant(Tensor::new(shape, data))
                }};
            }
            Ok(match first {
                Value::F(_) => join!(F),
                Value::I(_) => join!(I),
                Value::B(_) => join!(B),
            })
        }
        "Slice" => {
            let x = arg(node, args, 0)?;
            let rank = x.shape().len();
            let starts = arg(node, args, 1)?.ints(op)?;
            let ends = arg(node, args, 2)?.ints(op)?;
            let axes = match args.get(3).copied().flatten() {
                Some(a) => a.ints(op)?,
                None => (0..starts.len() as i64).collect(),
            };
            let steps = match args.get(4).copied().flatten() {
                Some(a) => a.ints(op)?,
                None => vec![1; starts.len()],
            };
            let mut lo: Vec<usize> = vec![0; rank];
            let mut step: Vec<usize> = vec![1; rank];
            let mut shape = x.shape().to_vec();
            for (k, a) in axes.iter().enumerate() {
                let a = axis(*a, rank);
                let dim = x.shape()[a] as i64;
                if steps[k] <= 0 {
                    return Err(err(op, "only positive steps"));
                }
                let clamp = |v: i64| {
                    let v = if v < 0 { v + dim } else { v };
                    v.clamp(0, dim)
                };
                let (s, e) = (clamp(starts[k]), clamp(ends[k]));
                lo[a] = s as usize;
                step[a] = steps[k] as usize;
                shape[a] = if e > s {
                    ((e - s + steps[k] - 1) / steps[k]) as usize
                } else {
                    0
                };
            }
            let st = strides(x.shape());
            let total: usize = shape.iter().product();
            let mut index = Vec::with_capacity(total);
            let mut counter = vec![0usize; rank];
            for _ in 0..total {
                let flat: usize = (0..rank)
                    .map(|d| (lo[d] + counter[d] * step[d]) * st[d])
                    .sum();
                index.push(flat);
                for d in (0..rank).rev() {
                    counter[d] += 1;
                    if counter[d] < shape[d] {
                        break;
                    }
                    counter[d] = 0;
                }
            }
            Ok(x.gather(shape, &index))
        }
        "Pad" => {
            let x = arg(node, args, 0)?.floats(op)?;
            let pads = arg(node, args, 1)?.ints(op)?;
            let value = match args.get(2).copied().flatten() {
                Some(Value::F(t)) => t.data.first().copied().unwrap_or(0.0),
                _ => 0.0,
            };
            let mode = node.text("mode").unwrap_or("constant");
            let rank = x.shape.len();
            if pads.len() != 2 * rank || pads.iter().any(|p| *p < 0) {
                return Err(err(op, format!("pads {pads:?}")));
            }
            let shape: Vec<usize> = (0..rank)
                .map(|d| x.shape[d] + (pads[d] + pads[d + rank]) as usize)
                .collect();
            let st = strides(&x.shape);
            let total: usize = shape.iter().product();
            let mut data = Vec::with_capacity(total);
            let mut counter = vec![0usize; rank];
            for _ in 0..total {
                let mut flat = 0usize;
                let mut inside = true;
                for d in 0..rank {
                    let mut i = counter[d] as i64 - pads[d];
                    let n = x.shape[d] as i64;
                    if i < 0 || i >= n {
                        match mode {
                            "reflect" if n > 1 => {
                                let period = 2 * (n - 1);
                                i = i.rem_euclid(period);
                                if i >= n {
                                    i = period - i;
                                }
                            }
                            "edge" => i = i.clamp(0, n - 1),
                            _ => inside = false,
                        }
                    }
                    if inside {
                        flat += i as usize * st[d];
                    }
                }
                data.push(if inside { x.data[flat] } else { value });
                for d in (0..rank).rev() {
                    counter[d] += 1;
                    if counter[d] < shape[d] {
                        break;
                    }
                    counter[d] = 0;
                }
            }
            Ok(Value::F(Tensor::new(shape, data)))
        }
        "Conv" => conv(node, args).map(Value::F),
        "Neg" | "Relu" | "Sigmoid" | "Sqrt" | "Log" => {
            let x = arg(node, args, 0)?.floats(op)?;
            let f: fn(f32) -> f32 = match op {
                "Neg" => |v| -v,
                "Relu" => |v| v.max(0.0),
                "Sigmoid" => |v| 1.0 / (1.0 + (-v).exp()),
                "Sqrt" => f32::sqrt,
                _ => f32::ln,
            };
            Ok(Value::F(Tensor::new(
                x.shape.clone(),
                x.data.iter().map(|v| f(*v)).collect(),
            )))
        }
        "Mul" | "Add" | "Sub" | "Div" => {
            let (a, b) = (arg(node, args, 0)?, arg(node, args, 1)?);
            match (a, b) {
                (Value::I(a), Value::I(b)) => {
                    let fa =
                        Tensor::new(a.shape.clone(), a.data.iter().map(|v| *v as f32).collect());
                    let fb =
                        Tensor::new(b.shape.clone(), b.data.iter().map(|v| *v as f32).collect());
                    let r = eval_f(node, &fa, &fb)?;
                    Ok(Value::I(Tensor::new(
                        r.shape,
                        r.data.iter().map(|v| *v as i64).collect(),
                    )))
                }
                _ => Ok(Value::F(eval_f(node, a.floats(op)?, b.floats(op)?)?)),
            }
        }
        "Equal" => {
            let (a, b) = (arg(node, args, 0)?, arg(node, args, 1)?);
            let (fa, fb) = (to_f(a), to_f(b));
            let r = binary_f(node, &fa, &fb, |x, y| if x == y { 1.0 } else { 0.0 })?;
            Ok(Value::B(Tensor::new(
                r.shape,
                r.data.iter().map(|v| *v != 0.0).collect(),
            )))
        }
        "Where" => {
            let c = match arg(node, args, 0)? {
                Value::B(t) => t.clone(),
                v => {
                    let f = to_f(v);
                    Tensor::new(f.shape, f.data.iter().map(|v| *v != 0.0).collect())
                }
            };
            let (x, y) = (to_f(arg(node, args, 1)?), to_f(arg(node, args, 2)?));
            let shape = broadcast(&broadcast(&c.shape, &x.shape, op)?, &y.shape, op)?;
            let (ic, ix, iy) = (
                broadcast_index(&c.shape, &shape),
                broadcast_index(&x.shape, &shape),
                broadcast_index(&y.shape, &shape),
            );
            let data = (0..ic.len())
                .map(|k| {
                    if c.data[ic[k]] {
                        x.data[ix[k]]
                    } else {
                        y.data[iy[k]]
                    }
                })
                .collect();
            Ok(Value::F(Tensor::new(shape, data)))
        }
        "Cast" => {
            let x = arg(node, args, 0)?;
            let f = to_f(x);
            Ok(match node.int("to", 1) {
                1 => Value::F(f),
                6 | 7 => Value::I(match x {
                    Value::I(t) => t.clone(),
                    _ => Tensor::new(f.shape, f.data.iter().map(|v| *v as i64).collect()),
                }),
                9 => Value::B(Tensor::new(
                    f.shape,
                    f.data.iter().map(|v| *v != 0.0).collect(),
                )),
                t => return Err(err(op, format!("to type {t}"))),
            })
        }
        "Shape" => {
            let x = arg(node, args, 0)?;
            let s: Vec<i64> = x.shape().iter().map(|d| *d as i64).collect();
            Ok(Value::I(Tensor::new(vec![s.len()], s)))
        }
        "ReduceSum" | "ReduceMin" | "ReduceMax" => {
            let x = arg(node, args, 0)?.floats(op)?;
            let rank = x.shape.len();
            let axes = match args.get(1).copied().flatten() {
                Some(a) => Some(a.ints(op)?),
                None => node.ints("axes"),
            };
            let axes: Vec<usize> = match axes {
                Some(a) if !a.is_empty() => a.iter().map(|v| axis(*v, rank)).collect(),
                _ if op == "ReduceSum" && node.int("noop_with_empty_axes", 0) == 1 => {
                    return Ok(Value::F(x.clone()));
                }
                _ => (0..rank).collect(),
            };
            let keep = node.int("keepdims", 1) == 1;
            let kept: Vec<usize> = (0..rank)
                .map(|d| if axes.contains(&d) { 1 } else { x.shape[d] })
                .collect();
            let (init, f): (f32, fn(f32, f32) -> f32) = match op {
                "ReduceSum" => (0.0, |a, b| a + b),
                "ReduceMin" => (f32::INFINITY, f32::min),
                _ => (f32::NEG_INFINITY, f32::max),
            };
            let total: usize = kept.iter().product();
            let mut out = vec![init; total];
            let ks = strides(&kept);
            let mut counter = vec![0usize; rank];
            for v in &x.data {
                let flat: usize = (0..rank)
                    .map(|d| {
                        if axes.contains(&d) {
                            0
                        } else {
                            counter[d] * ks[d]
                        }
                    })
                    .sum();
                out[flat] = f(out[flat], *v);
                for d in (0..rank).rev() {
                    counter[d] += 1;
                    if counter[d] < x.shape[d] {
                        break;
                    }
                    counter[d] = 0;
                }
            }
            let shape = if keep {
                kept
            } else {
                (0..rank)
                    .filter(|d| !axes.contains(d))
                    .map(|d| x.shape[d])
                    .collect()
            };
            Ok(Value::F(Tensor::new(shape, out)))
        }
        _ => Err(err(op, "not supported")),
    }
}

fn to_f(v: &Value) -> Tensor<f32> {
    match v {
        Value::F(t) => t.clone(),
        Value::I(t) => Tensor::new(t.shape.clone(), t.data.iter().map(|x| *x as f32).collect()),
        Value::B(t) => Tensor::new(
            t.shape.clone(),
            t.data.iter().map(|x| if *x { 1.0 } else { 0.0 }).collect(),
        ),
    }
}

fn eval_f(node: &Node, a: &Tensor<f32>, b: &Tensor<f32>) -> Result<Tensor<f32>, GraphError> {
    match node.op.as_str() {
        "Mul" => binary_f(node, a, b, |x, y| x * y),
        "Add" => binary_f(node, a, b, |x, y| x + y),
        "Sub" => binary_f(node, a, b, |x, y| x - y),
        _ => binary_f(node, a, b, |x, y| x / y),
    }
}

/// 2-D convolution, NCHW, weights (M, C/group, kH, kW).
fn conv(node: &Node, args: &[Option<&Value>]) -> Result<Tensor<f32>, GraphError> {
    let op = node.op.as_str();
    let x = arg(node, args, 0)?.floats(op)?;
    let w = arg(node, args, 1)?.floats(op)?;
    let bias = args
        .get(2)
        .copied()
        .flatten()
        .map(|b| b.floats(op))
        .transpose()?;
    if x.shape.len() != 4 || w.shape.len() != 4 {
        return Err(err(op, "only 2-D convolutions"));
    }
    if node.int("group", 1) != 1 {
        return Err(err(op, "only one group"));
    }
    let (n, c, h, wd) = (x.shape[0], x.shape[1], x.shape[2], x.shape[3]);
    let (m, wc, kh, kw) = (w.shape[0], w.shape[1], w.shape[2], w.shape[3]);
    if wc != c {
        return Err(err(op, format!("{c} channels, kernel for {wc}")));
    }
    let s = node.ints("strides").unwrap_or_else(|| vec![1, 1]);
    let d = node.ints("dilations").unwrap_or_else(|| vec![1, 1]);
    let p = node.ints("pads").unwrap_or_else(|| vec![0, 0, 0, 0]);
    if let Some(a) = node.text("auto_pad")
        && a != "NOTSET"
    {
        return Err(err(op, format!("auto_pad {a}")));
    }
    let (sh, sw, dh, dw) = (s[0] as usize, s[1] as usize, d[0] as usize, d[1] as usize);
    let (pt, pl, pb, pr) = (p[0] as usize, p[1] as usize, p[2] as usize, p[3] as usize);
    let oh = (h + pt + pb).saturating_sub(dh * (kh - 1) + 1) / sh + 1;
    let ow = (wd + pl + pr).saturating_sub(dw * (kw - 1) + 1) / sw + 1;
    let mut out = vec![0.0f32; n * m * oh * ow];
    for b in 0..n {
        for mo in 0..m {
            let o = &mut out[(b * m + mo) * oh * ow..(b * m + mo + 1) * oh * ow];
            if let Some(bias) = bias {
                o.fill(bias.data[mo]);
            }
            for ci in 0..c {
                let plane = &x.data[(b * c + ci) * h * wd..(b * c + ci + 1) * h * wd];
                for ki in 0..kh {
                    for kj in 0..kw {
                        let wv = w.data[((mo * c + ci) * kh + ki) * kw + kj];
                        if wv == 0.0 {
                            continue;
                        }
                        // Output columns whose input column is inside.
                        let off = (kj * dw) as i64 - pl as i64;
                        let j0 = if off < 0 {
                            ((-off) as usize).div_ceil(sw)
                        } else {
                            0
                        };
                        let j1 = if (wd as i64 - off) <= 0 {
                            0
                        } else {
                            (((wd as i64 - off - 1) as usize) / sw + 1).min(ow)
                        };
                        if j0 >= j1 {
                            continue;
                        }
                        for oi in 0..oh {
                            let ii = (oi * sh + ki * dh) as i64 - pt as i64;
                            if ii < 0 || ii >= h as i64 {
                                continue;
                            }
                            let row = &plane[ii as usize * wd..(ii as usize + 1) * wd];
                            let orow = &mut o[oi * ow..(oi + 1) * ow];
                            if sw == 1 {
                                let start = (j0 as i64 + off) as usize;
                                for (dst, src) in orow[j0..j1].iter_mut().zip(&row[start..]) {
                                    *dst += wv * src;
                                }
                            } else {
                                for (j, dst) in orow.iter_mut().enumerate().take(j1).skip(j0) {
                                    *dst += wv * row[(j as i64 * sw as i64 + off) as usize];
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(Tensor::new(vec![n, m, oh, ow], out))
}

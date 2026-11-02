use core_pipeline::params::*;
use serde_json::{json, Value};
use std::mem::{align_of, offset_of, size_of};

fn bytes<T: bytemuck::Pod>(v: &T) -> String {
    bytemuck::bytes_of(v)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn snapshot() -> Value {
    let inputs: Value = serde_json::from_str(include_str!(
        "../../../../tests/migration/model/inputs.json"
    ))
    .unwrap();
    let cases: Vec<Value> = inputs
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let p: DevelopParams = serde_json::from_value(case["params"].clone()).unwrap();
            json!({"name": case["name"], "resolved": p,
            "params": bytes(&p.to_uniform()), "extra": bytes(&p.to_extra([0.25, 0.125])),
            "wb": bytes(&p.to_wb_uniform()), "fx": bytes(&p.to_fx()),
            "masks": bytes(&p.to_mask_buffer()), "tone": bytes(&p.to_tone_op()),
            "geom": bytes(&p.to_geom(1.5, 1.0)), "cbrgb": bytes(&p.to_cbrgb()),
            "chanmix": bytes(&p.to_chanmix())})
        })
        .collect();
    macro_rules! layout {
        ($t:ty, $($f:ident),+) => { json!({"size":size_of::<$t>(), "align":align_of::<$t>(),
            "fields": {$(stringify!($f):offset_of!($t,$f)),+}}) };
    }
    json!({"cases":cases, "view": bytes(&ViewParams::full(0,0).to_uniform()), "layout": {
        "ParamsUniform":layout!(ParamsUniform,wb_gain,exposure,saturation,contrast,highlights,shadows,blacks,whites,_pad0,_pad1),
        "MaskParamsUniform":layout!(MaskParamsUniform,wb_gain,exposure,contrast,saturation,highlights,shadows,blacks,whites,opacity,enabled),
        "MaskBufferUniform":layout!(MaskBufferUniform,count,_pad,masks),
        "FxUniform":layout!(FxUniform,hsl), "WbUniform":layout!(WbUniform,cols),
        "ExtraUniform":layout!(ExtraUniform,detail,texel,local), "ToneOpUniform":layout!(ToneOpUniform,params),
        "GeomUniform":layout!(GeomUniform,crop,rot,aspect,lens), "ViewUniform":layout!(ViewUniform,rect,flags,color),
        "CbRgbUniform":layout!(CbRgbUniform,fwd,inv,global,shadows,midtones,highlights,params),
        "ChanMixUniform":layout!(ChanMixUniform,r,g,b,params)
    }})
}

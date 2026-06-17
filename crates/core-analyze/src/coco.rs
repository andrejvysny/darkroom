//! COCO 80-class labels (D-FINE / RT-DETR order) and the mapping to the user-facing target buckets.
//!
//! D-FINE owns **People** and **Vehicles** only. **Animals** are owned by the dedicated MegaDetector
//! analyzer (purpose-built, far better wildlife coverage + vegetation hard-negatives), so D-FINE's
//! 10 COCO animal classes — and the notorious `teddy bear` false-positive magnet — are intentionally
//! NOT mapped here. Other COCO classes map to `None` and are dropped by the object detector.

#[rustfmt::skip]
pub const COCO_LABELS: [&str; 80] = [
    "person","bicycle","car","motorbike","aeroplane","bus","train","truck","boat","traffic light",
    "fire hydrant","stop sign","parking meter","bench","bird","cat","dog","horse","sheep","cow",
    "elephant","bear","zebra","giraffe","backpack","umbrella","handbag","tie","suitcase","frisbee",
    "skis","snowboard","sports ball","kite","baseball bat","baseball glove","skateboard","surfboard",
    "tennis racket","bottle","wine glass","cup","fork","knife","spoon","bowl","banana","apple",
    "sandwich","orange","broccoli","carrot","hot dog","pizza","donut","cake","chair","sofa",
    "pottedplant","bed","diningtable","toilet","tvmonitor","laptop","mouse","remote","keyboard",
    "cell phone","microwave","oven","toaster","sink","refrigerator","book","clock","vase","scissors",
    "teddy bear","hair drier","toothbrush",
];

/// Target buckets surfaced in the UI. ("Animals" is populated by the MegaDetector analyzer.)
pub const CATEGORIES: [&str; 3] = ["People", "Animals", "Vehicles"];

// "car" plus other road vehicles.
const VEHICLES: &[&str] = &["bicycle", "car", "motorbike", "motorcycle", "bus", "truck"];

/// Map a COCO label to a D-FINE-owned bucket (People / Vehicles), or `None` otherwise.
/// Animals are owned by MegaDetector — see module docs.
pub fn category(label: &str) -> Option<&'static str> {
    if label == "person" {
        Some("People")
    } else if VEHICLES.contains(&label) {
        Some("Vehicles")
    } else {
        None
    }
}

/// Per-category accept threshold on the sigmoid score (balanced precision/recall). `person` is COCO's
/// most-fired class so it needs a higher gate; vehicles a touch lower. Applied AFTER [`category`].
pub fn threshold(category: &str) -> f32 {
    match category {
        "People" => 0.55,
        "Vehicles" => 0.50,
        _ => 0.60,
    }
}

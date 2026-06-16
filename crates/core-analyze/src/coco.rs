//! COCO 80-class labels (D-FINE / RT-DETR order) and the mapping to the user-facing target buckets:
//! People / Animals / Vehicles. Only these three categories are surfaced; other COCO classes are
//! still detected but map to `None` and are dropped by the object-detection analyzer.

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

/// Target buckets surfaced in the UI.
pub const CATEGORIES: [&str; 3] = ["People", "Animals", "Vehicles"];

const ANIMALS: &[&str] = &[
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "teddy bear",
];
// "car" plus other road vehicles. ("teddy bear" intentionally excluded here.)
const VEHICLES: &[&str] = &["bicycle", "car", "motorbike", "motorcycle", "bus", "truck"];

/// Map a COCO label to a target bucket, or `None` if it is not a People/Animals/Vehicles class.
pub fn category(label: &str) -> Option<&'static str> {
    if label == "person" {
        Some("People")
    } else if ANIMALS.contains(&label) {
        Some("Animals")
    } else if VEHICLES.contains(&label) {
        Some("Vehicles")
    } else {
        None
    }
}

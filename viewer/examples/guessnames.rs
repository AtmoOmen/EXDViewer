//! Names the curated table does not carry, tried against the ids that want them. A crc32 match is
//! conclusive; a miss says only that this guess was wrong.

const WANTED: [u32; 6] = [
    0x3b44_510e,
    0x948f_80ad,
    0x8187_d13f,
    0x2651_f93b,
    0x94a1_94c2,
    0x3a31_0f21,
];

const STEMS: [&str; 26] = [
    "Shadow",
    "ShadowMask",
    "ShadowMap",
    "LightShadow",
    "DirectionalShadow",
    "Occlusion",
    "AmbientOcclusion",
    "Mask",
    "LightDiffuse",
    "LightSpecular",
    "Light",
    "Depth",
    "Normal",
    "Attenuation",
    "Dither",
    "Noise",
    "Caustics",
    "CloudShadow",
    "SkyOcclusion",
    "Distortion",
    "Fresnel",
    "GBuffer3",
    "GBuffer4",
    "ViewPosition",
    "ShadowDistanceFade",
    "OmniShadowIndexTable",
];

const SHAPES: [&str; 6] = [
    "g_Sampler{}",
    "g_{}Sampler",
    "g_{}",
    "g_Texture{}",
    "{}",
    "g_Sampler{}Map",
];

fn main() {
    for id in WANTED {
        let mut found = Vec::new();
        for stem in STEMS {
            for shape in SHAPES {
                let name = shape.replace("{}", stem);
                if shaders::names::hash(name.as_bytes()) == id {
                    found.push(name);
                }
            }
        }
        match found.is_empty() {
            true => println!("{id:08x}  -"),
            false => println!("{id:08x}  {}", found.join(", ")),
        }
    }
}

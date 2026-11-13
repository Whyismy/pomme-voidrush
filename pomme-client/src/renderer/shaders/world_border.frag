#version 450

layout(set = 1, binding = 0) uniform texture2D forcefield_tex;
layout(set = 1, binding = 1) uniform sampler forcefield_sampler;

layout(push_constant) uniform BorderPush {
    vec4 tint;
};

layout(location = 0) in vec2 v_uv;

layout(location = 0) out vec4 out_color;

void main() {
    // The forcefield, repeating, in the border's colour.
    vec4 color = texture(sampler2D(forcefield_tex, forcefield_sampler), v_uv) * tint;
    if (color.a == 0.0) discard;
    out_color = color;
}

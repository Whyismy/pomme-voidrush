#version 450

// Vanilla `rendertype_end_portal.vsh`: the face in clip space, and its
// screen position for the fragment shader's projected layers.

layout(set = 0, binding = 0) uniform CameraUniform {
    mat4 view_proj;
    vec4 camera_pos;
    vec4 fog_color;
};

layout(location = 0) in vec3 position;
layout(location = 1) in float layers;

layout(location = 0) out vec4 tex_proj;
layout(location = 1) flat out int portal_layers;

// Vanilla `projection_from_position` (projection.glsl).
vec4 projection_from_position(vec4 pos) {
    vec4 projection = pos * 0.5;
    projection.xy = vec2(projection.x + projection.w, projection.y + projection.w);
    projection.zw = pos.zw;
    return projection;
}

void main() {
    // Anchor-relative in, eye-relative for `view_proj`.
    gl_Position = view_proj * vec4(position - camera_pos.xyz, 1.0);
    tex_proj = projection_from_position(gl_Position);
    portal_layers = int(layers);
}

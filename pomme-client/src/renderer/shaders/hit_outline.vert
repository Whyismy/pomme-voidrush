#version 450

// Vanilla `rendertype_lines.vsh`: each edge is a quad, its two sides pushed
// apart on screen to the line width, and the whole outline pulled a 256th of
// the way toward the eye so it never sinks into the block it outlines.

layout(set = 0, binding = 0) uniform CameraUniform {
    mat4 view_proj;
    vec4 camera_pos;
    vec4 fog_color;
};

layout(push_constant) uniform Push {
    vec2 screen_size;
    float line_width;
    float _pad;
    vec4 color;
};

layout(location = 0) in vec3 position;
layout(location = 1) in vec3 direction;
layout(location = 2) in float side;

layout(location = 0) out vec4 v_color;

const float VIEW_SHRINK = 1.0 - (1.0 / 256.0);

void main() {
    vec3 rel = position - camera_pos.xyz;
    vec4 start = view_proj * vec4(rel * VIEW_SHRINK, 1.0);
    vec4 end = view_proj * vec4((rel + direction) * VIEW_SHRINK, 1.0);
    vec3 ndc1 = start.xyz / start.w;
    vec3 ndc2 = end.xyz / end.w;
    vec2 screen_direction = normalize((ndc2.xy - ndc1.xy) * screen_size);
    vec2 offset = vec2(-screen_direction.y, screen_direction.x) * line_width / screen_size;
    if (offset.x < 0.0) {
        offset *= -1.0;
    }
    gl_Position = vec4((ndc1 + vec3(offset * side, 0.0)) * start.w, start.w);
    v_color = color;
}

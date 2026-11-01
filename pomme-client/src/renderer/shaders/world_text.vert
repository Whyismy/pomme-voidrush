#version 450

// Text displays: menu_overlay's vertices placed in the world (anchor-relative,
// like the item pipelines) and lit like vanilla's text render types, which
// multiply the vertex colour by the lightmap. menu_overlay.frag shades them.

#include "camera_ubo.glsl"

// Sets 0 and 1 are the camera and menu_overlay's textures.
#define LIGHTMAP_SET 2
#include "lightmap.glsl"

layout(location = 0) in vec3 pos;
layout(location = 1) in vec2 uv;
layout(location = 2) in vec4 color;
layout(location = 3) in float mode;
layout(location = 4) in vec2 rect_size;
layout(location = 5) in float corner_radius;
layout(location = 6) in float light;

layout(location = 0) out vec2 v_uv;
layout(location = 1) out vec4 v_color;
layout(location = 2) out float v_mode;
layout(location = 3) out vec2 v_rect_size;
layout(location = 4) out float v_corner_radius;

void main() {
    gl_Position = view_proj * vec4(pos - camera_pos.xyz, 1.0);
    v_uv = uv;
    v_color = vec4(color.rgb * light_lane_color(light).rgb, color.a);
    v_mode = mode;
    v_rect_size = rect_size;
    v_corner_radius = corner_radius;
}

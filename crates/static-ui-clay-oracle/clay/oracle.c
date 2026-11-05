// Builds a flat, pre-order tree description through Clay and reports every
// container's box and every text render command, for static-ui-layout's
// differential test. Text is measured by `oracle_measure_width`, which the
// Rust side calls too, so both measure identically.
#define CLAY_IMPLEMENTATION
#include "clay.h"
#include <stdlib.h>

typedef struct {
    int32_t kind; // 0: container, 1: text
    int32_t child_count; // declared children (text and floating included)
    int32_t width_type;
    float width_min;
    float width_max;
    int32_t height_type;
    float height_min;
    float height_max;
    uint16_t pad_left, pad_right, pad_top, pad_bottom, gap;
    uint8_t align_x, align_y, direction;
    int32_t attach_to; // Clay_FloatingAttachToElement
    int32_t attach_element; // node index, for ATTACH_TO_ELEMENT_WITH_ID
    float offset_x, offset_y, expand_w, expand_h;
    int16_t z;
    uint8_t element_point, parent_point, clip_to;
    uint8_t clip_h, clip_v;
    float child_offset_x, child_offset_y;
    float aspect;
    int32_t text_start, text_len;
    uint16_t font, size, letter_spacing, line_height;
    uint8_t wrap, text_align;
} OracleNode;

typedef struct {
    float x, y, width, height;
    int32_t length;
} OracleLine;

static const OracleNode *nodes;
static const char *texts;
static int32_t errors;
static void *memory;

float oracle_measure_width(const char *chars, int32_t length, uint16_t size, uint16_t letter_spacing) {
    float width = 0;
    for (int32_t i = 0; i < length; i++) {
        char c = chars[i];
        width += (c == 'i' || c == 'l' || c == '.') ? (float)size * 0.25f : (float)size * 0.5f;
        width += (float)letter_spacing;
    }
    return width;
}
float oracle_line_height(uint16_t size) {
    return (float)size * 1.25f;
}
static Clay_Dimensions measure(Clay_StringSlice text, Clay_TextElementConfig *config, void *user) {
    (void)user;
    return (Clay_Dimensions){
        oracle_measure_width(text.chars, text.length, config->fontSize, config->letterSpacing),
        oracle_line_height(config->fontSize),
    };
}
static void on_error(Clay_ErrorData error) {
    (void)error;
    errors++;
}

void oracle_init(int32_t max_elements) {
    Clay_SetMaxElementCount(max_elements);
    uint32_t size = Clay_MinMemorySize();
    free(memory);
    memory = malloc(size);
    Clay_Arena arena = Clay_CreateArenaWithCapacityAndMemory(size, memory);
    Clay_Initialize(arena, (Clay_Dimensions){800, 600}, (Clay_ErrorHandler){on_error, 0});
    Clay_SetMeasureTextFunction(measure, 0);
    Clay_SetCullingEnabled(false);
}

static Clay_SizingAxis sizing(int32_t type, float min, float max) {
    Clay_SizingAxis axis = {0};
    axis.type = (Clay__SizingType)type;
    if (type == CLAY__SIZING_TYPE_PERCENT) {
        axis.size.percent = min;
    } else {
        axis.size.minMax.min = min;
        axis.size.minMax.max = max;
    }
    return axis;
}

static int32_t build(int32_t i) {
    const OracleNode *n = &nodes[i];
    if (n->kind == 1) {
        Clay_String text = {.isStaticallyAllocated = false, .length = n->text_len, .chars = texts + n->text_start};
        Clay_TextElementConfig config = {0};
        config.fontId = n->font;
        config.fontSize = n->size;
        config.letterSpacing = n->letter_spacing;
        config.lineHeight = n->line_height;
        config.wrapMode = (Clay_TextElementConfigWrapMode)n->wrap;
        config.textAlignment = (Clay_TextAlignment)n->text_align;
        config.textColor = (Clay_Color){0, 0, 0, 255};
        Clay__OpenTextElement(text, config);
        return i + 1;
    }
    Clay__OpenElementWithId(CLAY_IDI("e", i));
    Clay_ElementDeclaration d = {0};
    d.layout.sizing.width = sizing(n->width_type, n->width_min, n->width_max);
    d.layout.sizing.height = sizing(n->height_type, n->height_min, n->height_max);
    d.layout.padding = (Clay_Padding){n->pad_left, n->pad_right, n->pad_top, n->pad_bottom};
    d.layout.childGap = n->gap;
    d.layout.childAlignment.x = (Clay_LayoutAlignmentX)n->align_x;
    d.layout.childAlignment.y = (Clay_LayoutAlignmentY)n->align_y;
    d.layout.layoutDirection = (Clay_LayoutDirection)n->direction;
    d.backgroundColor = (Clay_Color){10, 10, 10, 255};
    if (n->attach_to != CLAY_ATTACH_TO_NONE) {
        d.floating.attachTo = (Clay_FloatingAttachToElement)n->attach_to;
        if (n->attach_to == CLAY_ATTACH_TO_ELEMENT_WITH_ID) {
            d.floating.parentId = CLAY_IDI("e", n->attach_element).id;
        }
        d.floating.offset = (Clay_Vector2){n->offset_x, n->offset_y};
        d.floating.expand = (Clay_Dimensions){n->expand_w, n->expand_h};
        d.floating.zIndex = n->z;
        d.floating.attachPoints.element = (Clay_FloatingAttachPointType)n->element_point;
        d.floating.attachPoints.parent = (Clay_FloatingAttachPointType)n->parent_point;
        d.floating.clipTo = (Clay_FloatingClipToElement)n->clip_to;
    }
    d.clip.horizontal = n->clip_h;
    d.clip.vertical = n->clip_v;
    d.clip.childOffset = (Clay_Vector2){n->child_offset_x, n->child_offset_y};
    d.aspectRatio.aspectRatio = n->aspect;
    Clay__ConfigureOpenElement(d);
    int32_t next = i + 1;
    for (int32_t c = 0; c < n->child_count; c++) {
        next = build(next);
    }
    Clay__CloseElement();
    return next;
}

// Lays out `count` nodes (top-level ones are the root's children) in a
// `width` x `height` root. Fills `boxes[i]` for every container node,
// and up to `max_lines` text commands; returns how many, or -1 per error.
int32_t oracle_layout(const OracleNode *all, int32_t count, const char *text, float width, float height,
                      Clay_BoundingBox *boxes, OracleLine *lines, int32_t max_lines, int32_t *error_count) {
    nodes = all;
    texts = text;
    errors = 0;
    Clay_SetLayoutDimensions((Clay_Dimensions){width, height});
    Clay_BeginLayout();
    int32_t next = 0;
    while (next < count) {
        next = build(next);
    }
    Clay_RenderCommandArray commands = Clay_EndLayout(0);
    for (int32_t i = 0; i < count; i++) {
        if (all[i].kind == 0) {
            Clay_ElementData data = Clay_GetElementData(CLAY_IDI("e", i));
            boxes[i] = data.found ? data.boundingBox : (Clay_BoundingBox){-1, -1, -1, -1};
        }
    }
    int32_t written = 0;
    for (int32_t c = 0; c < commands.length; c++) {
        Clay_RenderCommand *command = &commands.internalArray[c];
        if (command->commandType != CLAY_RENDER_COMMAND_TYPE_TEXT || written >= max_lines) {
            continue;
        }
        Clay_BoundingBox b = command->boundingBox;
        lines[written++] = (OracleLine){b.x, b.y, b.width, b.height, command->renderData.text.stringContents.length};
    }
    *error_count = errors;
    return written;
}

// Just the layout, for timing against static-ui-layout.
void oracle_layout_only(const OracleNode *all, int32_t count, const char *text, float width, float height) {
    nodes = all;
    texts = text;
    Clay_SetLayoutDimensions((Clay_Dimensions){width, height});
    Clay_BeginLayout();
    int32_t next = 0;
    while (next < count) {
        next = build(next);
    }
    Clay_EndLayout(0);
}

#import "megamap.h"
#import <AppKit/AppKit.h>
#import <Metal/Metal.h>
#import <QuartzCore/CAMetalLayer.h>
#import <CoreText/CoreText.h>
#import <ImageIO/ImageIO.h>
#include <math.h>
#include <string.h>

_Static_assert(sizeof(MmQuad) == 96, "Metal instance stride");
static const NSUInteger MmAtlasSize = 4096;
static const NSUInteger MmMaxInstances = 500000;
static NSString *MmStartupError = @"";
static NSData *MmStartupErrorBytes;

@class MmState;
@interface MmQueuedEvent : NSObject
@property MmEvent event;
@property(strong) NSString *text;
@property(strong) NSData *bytes;
@end
@implementation MmQueuedEvent
@end

@interface MmSlot : NSObject
@property(strong) id<MTLBuffer> buffer;
@property(strong) id<MTLCommandBuffer> command;
@property NSUInteger capacity;
@end
@implementation MmSlot
@end

@interface MmGlyph : NSObject
@property NSUInteger x, y, width, height;
@property double originX, originY, rasterSize;
@end
@implementation MmGlyph
@end

@interface MmLine : NSObject
@property CTLineRef line;
@property double width, ascent;
@property(strong) NSMutableData *renderQuads;
@property(strong) NSString *cacheKey;
@property NSUInteger sourceLength;
@property NSUInteger generation, missingGlyphs;
@property double rasterScale;
@end
@implementation MmLine
- (void)dealloc { if (_line) CFRelease(_line); }
@end

@interface MmView : NSView<NSTextInputClient>
@property(weak) MmState *state;
@property(strong) NSTrackingArea *tracking;
@property(strong) NSString *input, *marked;
@property NSRange selection, markedSelection, compositionReplace;
@property NSRect caret;
@end

@interface MmAppDelegate : NSObject<NSApplicationDelegate>
@property(weak) MmState *state;
@end

@interface MmAXElement : NSAccessibilityElement
@property(weak) MmState *state;
@property uint64_t controlId;
@end

@interface MmState : NSObject<NSWindowDelegate>
@property(strong) NSWindow *window;
@property(strong) MmView *view;
@property(strong) MmAppDelegate *appDelegate;
@property(strong) CAMetalLayer *layer;
@property(strong) id<MTLDevice> device;
@property(strong) id<MTLCommandQueue> queue;
@property(strong) id<MTLRenderPipelineState> pipeline;
@property(strong) id<MTLTexture> atlas, target;
@property(strong) NSMutableData *instances;
@property(strong) NSMutableData *atlasPixels;
@property(strong) NSMutableDictionary<NSString *, MmGlyph *> *glyphs;
@property(strong) NSCache<NSString *, MmLine *> *lines;
@property(strong) NSArray<MmSlot *> *slots;
@property(strong) NSMutableArray<MmQueuedEvent *> *events;
@property(strong) MmQueuedEvent *lastEvent;
@property(strong) NSString *error, *clipboard;
@property(strong) NSData *borrowedBytes;
@property(strong) NSArray<MmAXElement *> *accessibility;
@property NSUInteger atlasX, atlasY, atlasRow, atlasUsed, slotIndex;
@property NSUInteger dirtyMinY, dirtyMaxY;
@property NSUInteger generation;
@property BOOL hidden, resetAtlas, closed, inputOverflow;
@property double started, width, height, scale;
@property MTLClearColor clearColor;
@property MmFrameStats stats;
- (void)push:(MmEvent)event text:(NSString *)text;
- (void)sizeChanged;
@end

static MmState *mm_state(MmContext *context) { return (__bridge MmState *)context; }
static uint32_t mm_modifiers(NSEventModifierFlags flags) {
    return ((flags & NSEventModifierFlagShift) ? 1 : 0) |
           ((flags & NSEventModifierFlagControl) ? 2 : 0) |
           ((flags & NSEventModifierFlagOption) ? 4 : 0) |
           ((flags & NSEventModifierFlagCommand) ? 8 : 0);
}

@implementation MmState
- (void)push:(MmEvent)event text:(NSString *)text {
    if (_closed) return;
    if (event.kind == 1 && _events.lastObject.event.kind == 1) [_events removeLastObject];
    if (_events.count >= 4096) {
        NSUInteger move = [_events indexOfObjectPassingTest:^BOOL(MmQueuedEvent *value, NSUInteger index, BOOL *stop) {
            (void)index; (void)stop; return value.event.kind == 1;
        }];
        if (move != NSNotFound) [_events removeObjectAtIndex:move];
        else { _error = @"Native input queue is full"; _inputOverflow = YES; return; }
    }
    MmQueuedEvent *queued = [MmQueuedEvent new];
    queued.event = event;
    queued.text = text ?: @"";
    queued.bytes = [queued.text dataUsingEncoding:NSUTF8StringEncoding];
    [_events addObject:queued];
}
- (void)sizeChanged {
    _width = _view.bounds.size.width;
    _height = _view.bounds.size.height;
    _scale = _window.backingScaleFactor;
    if (_scale <= 0) _scale = 1;
    _layer.contentsScale = _scale;
    _layer.drawableSize = CGSizeMake(MAX(1, ceil(_width * _scale)), MAX(1, ceil(_height * _scale)));
    MmEvent event = {0}; event.kind = 8;
    event.width = _width; event.height = _height; event.scale = _scale;
    [self push:event text:nil];
}
- (void)windowDidResize:(NSNotification *)notification { (void)notification; [self sizeChanged]; }
- (void)windowDidChangeBackingProperties:(NSNotification *)notification { (void)notification; [self sizeChanged]; }
- (BOOL)windowShouldClose:(NSWindow *)sender {
    (void)sender; MmEvent event = {0}; event.kind = 9; [self push:event text:nil]; return NO;
}
@end

@implementation MmAppDelegate
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    (void)sender; MmEvent event = {0}; event.kind = 9; [_state push:event text:nil]; return NSTerminateCancel;
}
@end

@implementation MmAXElement
- (BOOL)accessibilityPerformPress {
    if (!self.accessibilityEnabled || !_state) return NO;
    MmEvent event = {0}; event.kind = 10; event.id = _controlId;
    [_state push:event text:nil]; return YES;
}
@end

@implementation MmView
- (BOOL)isFlipped { return YES; }
- (BOOL)acceptsFirstResponder { return YES; }
- (BOOL)acceptsFirstMouse:(NSEvent *)event { (void)event; return YES; }
- (void)updateTrackingAreas {
    [super updateTrackingAreas];
    if (_tracking) [self removeTrackingArea:_tracking];
    _tracking = [[NSTrackingArea alloc] initWithRect:NSZeroRect options:NSTrackingMouseMoved | NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways | NSTrackingInVisibleRect owner:self userInfo:nil];
    [self addTrackingArea:_tracking];
}
- (void)pointer:(NSEvent *)input kind:(uint32_t)kind {
    NSPoint p = [self convertPoint:input.locationInWindow fromView:nil];
    MmEvent event = {0}; event.kind = kind; event.x = p.x; event.y = p.y;
    if (kind == 2 || kind == 3) { event.button = (uint32_t)input.buttonNumber; event.clicks = (uint32_t)input.clickCount; }
    event.modifiers = mm_modifiers(input.modifierFlags);
    [_state push:event text:nil];
}
- (void)mouseMoved:(NSEvent *)event { [self pointer:event kind:1]; }
- (void)mouseEntered:(NSEvent *)event { [self pointer:event kind:1]; }
- (void)mouseExited:(NSEvent *)event { [self pointer:event kind:1]; }
- (void)mouseDragged:(NSEvent *)event { [self pointer:event kind:1]; }
- (void)rightMouseDragged:(NSEvent *)event { [self pointer:event kind:1]; }
- (void)otherMouseDragged:(NSEvent *)event { [self pointer:event kind:1]; }
- (void)mouseDown:(NSEvent *)event { [self.window makeFirstResponder:self]; [self pointer:event kind:2]; }
- (void)rightMouseDown:(NSEvent *)event { [self pointer:event kind:2]; }
- (void)otherMouseDown:(NSEvent *)event { [self pointer:event kind:2]; }
- (void)mouseUp:(NSEvent *)event { [self pointer:event kind:3]; }
- (void)rightMouseUp:(NSEvent *)event { [self pointer:event kind:3]; }
- (void)otherMouseUp:(NSEvent *)event { [self pointer:event kind:3]; }
- (void)scrollWheel:(NSEvent *)input {
    NSPoint p = [self convertPoint:input.locationInWindow fromView:nil];
    MmEvent event = {0}; event.kind = 4; event.x = p.x; event.y = p.y;
    event.dx = input.scrollingDeltaX; event.dy = input.scrollingDeltaY;
    if (!input.hasPreciseScrollingDeltas) { event.dx *= 10; event.dy *= 10; }
    event.modifiers = mm_modifiers(input.modifierFlags);
    [_state push:event text:nil];
}
- (void)key:(uint32_t)code modifiers:(uint32_t)modifiers {
    MmEvent event = {0}; event.kind = 5; event.code = code; event.modifiers = modifiers;
    [_state push:event text:nil];
}
- (void)keyDown:(NSEvent *)input {
    uint32_t mods = mm_modifiers(input.modifierFlags);
    NSString *characters = input.charactersIgnoringModifiers.lowercaseString;
    if ((mods & 8) && characters.length) {
        unichar c = [characters characterAtIndex:0];
        switch (c) {
            case NSLeftArrowFunctionKey: case NSUpArrowFunctionKey: [self key:0x104 modifiers:mods]; return;
            case NSRightArrowFunctionKey: case NSDownArrowFunctionKey: [self key:0x105 modifiers:mods]; return;
            case 'a': [self key:0x200 modifiers:mods]; return;
            case 'c': [self key:0x201 modifiers:mods]; return;
            case 'x': [self key:0x202 modifiers:mods]; return;
            case 'v': [self key:0x203 modifiers:mods]; return;
            case 'z': [self key:0x204 modifiers:mods]; return;
            case 'q': case 'w': { MmEvent event = {0}; event.kind = 9; [_state push:event text:nil]; return; }
            default: [self key:c modifiers:mods]; return;
        }
    }
    switch (input.keyCode) {
        case 53:
            if (_marked.length) {
                MmEvent event = {0}; event.kind = 7; event.replace_start = _compositionReplace.location; event.replace_len = _compositionReplace.length;
                _marked = @""; [self.inputContext discardMarkedText]; [_state push:event text:@""];
            } else [self key:27 modifiers:mods];
            return;
        case 36: case 76:
            if (_marked.length) [self interpretKeyEvents:@[input]];
            else [self key:13 modifiers:mods];
            return;
        case 48: [self key:9 modifiers:mods]; return;
        default: [self interpretKeyEvents:@[input]];
    }
}
- (void)doCommandBySelector:(SEL)selector {
    NSString *name = NSStringFromSelector(selector);
    NSDictionary *codes = @{ @"deleteBackward:":@8, @"deleteForward:":@0x106,
        @"moveLeft:":@0x100, @"moveRight:":@0x101, @"moveUp:":@0x102, @"moveDown:":@0x103,
        @"moveToBeginningOfLine:":@0x104, @"moveToEndOfLine:":@0x105,
        @"insertNewline:":@13, @"insertTab:":@9, @"cancelOperation:":@27,
        @"moveLeftAndModifySelection:":@0x100, @"moveRightAndModifySelection:":@0x101,
        @"moveToBeginningOfLineAndModifySelection:":@0x104, @"moveToEndOfLineAndModifySelection:":@0x105 };
    if ([name hasPrefix:@"deleteWordBackward"]) name = @"deleteBackward:";
    else if ([name hasPrefix:@"deleteWordForward"]) name = @"deleteForward:";
    else if ([name hasPrefix:@"moveWordLeft"]) name = @"moveLeft:";
    else if ([name hasPrefix:@"moveWordRight"]) name = @"moveRight:";
    NSNumber *code = codes[name];
    if (code) [self key:code.unsignedIntValue modifiers:mm_modifiers(NSApp.currentEvent.modifierFlags)];
}
- (void)insertText:(id)value replacementRange:(NSRange)replacement {
    NSString *text = [value isKindOfClass:NSAttributedString.class] ? [value string] : value;
    if (![text isKindOfClass:NSString.class]) return;
    NSRange range = _marked.length ? _compositionReplace : (replacement.location == NSNotFound ? _selection : replacement);
    MmEvent event = {0}; event.kind = 6; event.replace_start = range.location; event.replace_len = range.length;
    _marked = @"";
    [_state push:event text:text];
}
- (void)setMarkedText:(id)value selectedRange:(NSRange)selected replacementRange:(NSRange)replacement {
    NSString *text = [value isKindOfClass:NSAttributedString.class] ? [value string] : value;
    if (![text isKindOfClass:NSString.class]) return;
    if (!_marked.length) _compositionReplace = replacement.location == NSNotFound ? _selection : replacement;
    _marked = text; _markedSelection = selected;
    MmEvent event = {0}; event.kind = 7; event.selection_start = selected.location; event.selection_len = selected.length;
    event.replace_start = _compositionReplace.location; event.replace_len = _compositionReplace.length;
    [_state push:event text:text];
}
- (void)unmarkText {
    if (_marked.length) [self insertText:_marked replacementRange:_compositionReplace];
}
- (NSRange)selectedRange { return _marked.length ? NSMakeRange(_compositionReplace.location + _markedSelection.location, _markedSelection.length) : _selection; }
- (NSRange)markedRange { return _marked.length ? NSMakeRange(_compositionReplace.location, _marked.length) : NSMakeRange(NSNotFound, 0); }
- (BOOL)hasMarkedText { return _marked.length > 0; }
- (NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range actualRange:(NSRangePointer)actual {
    NSString *text = _input ?: @"";
    if (_marked.length && NSMaxRange(_compositionReplace) <= text.length) text = [text stringByReplacingCharactersInRange:_compositionReplace withString:_marked];
    if (range.location > text.length) return nil;
    range.length = MIN(range.length, text.length - range.location);
    if (actual) *actual = range;
    return [[NSAttributedString alloc] initWithString:[text substringWithRange:range]];
}
- (NSArray<NSAttributedStringKey> *)validAttributesForMarkedText { return @[]; }
- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actual {
    if (actual) *actual = range;
    return [self.window convertRectToScreen:[self convertRect:_caret toView:nil]];
}
- (NSUInteger)characterIndexForPoint:(NSPoint)point { (void)point; return self.selectedRange.location; }
@end

static const char *MmShader =
"#include <metal_stdlib>\nusing namespace metal;\n"
"struct Q { float4 rect,uv,color,clip,edge,params; };\n"
"struct V { float4 position [[position]]; float2 local; uint instance [[flat]]; };\n"
"vertex V mm_vertex(uint v [[vertex_id]], uint i [[instance_id]], device const Q *q [[buffer(0)]], constant float2 &view [[buffer(1)]]) {\n"
" const float2 p[6] = {float2(0,0),float2(1,0),float2(0,1),float2(0,1),float2(1,0),float2(1,1)};\n"
" float2 xy=q[i].rect.xy+p[v]*q[i].rect.zw; V o; o.position=float4(xy.x/view.x*2-1,1-xy.y/view.y*2,0,1); o.local=p[v]; o.instance=i; return o; }\n"
"fragment float4 mm_fragment(V in [[stage_in]], device const Q *q [[buffer(0)]], texture2d<float> atlas [[texture(0)]]) {\n"
" Q a=q[in.instance]; float2 xy=a.rect.xy+in.local*a.rect.zw; if(any(xy<a.clip.xy)||any(xy>=a.clip.xy+a.clip.zw)) discard_fragment();\n"
" float4 c=a.color; if(a.params.z>1.5) { if(in.local.y<abs(in.local.x-0.5)*2.0) discard_fragment(); }\n"
" else if(a.params.z>0.5) { constexpr sampler s(coord::normalized,address::clamp_to_edge,filter::linear); c.a*=atlas.sample(s,a.uv.xy+in.local*a.uv.zw).r; }\n"
" else { float2 d=min(in.local,1-in.local)*a.rect.zw; if(min(d.x,d.y)<a.params.x) c=a.edge; else if(a.params.y>0.0) c.rgb*=1.0+0.3*step(0.5,fract((xy.x+xy.y)/10.0))*a.params.y; }\n"
" return float4(c.rgb*c.a,c.a); }\n";

static NSString *mm_string(const uint8_t *bytes, size_t length) {
    if (!bytes && length) return @"";
    return [[NSString alloc] initWithBytes:bytes length:length encoding:NSUTF8StringEncoding] ?: @"";
}

static MmLine *mm_line(MmState *state, NSString *text, double size, BOOL mono) {
    size = fmax(1, fmin(1024, size));
    NSString *key = [NSString stringWithFormat:@"%d/%.3f/%@", mono, size, text];
    MmLine *cached = [state.lines objectForKey:key];
    if (cached) return cached;
    CTFontRef font = CTFontCreateWithName(mono ? CFSTR("Menlo") : CFSTR("Helvetica"), size, NULL);
    NSDictionary *attributes = @{ (__bridge NSString *)kCTFontAttributeName:(__bridge id)font,
                                  (__bridge NSString *)kCTLigatureAttributeName:@0 };
    NSAttributedString *attributed = [[NSAttributedString alloc] initWithString:text attributes:attributes];
    MmLine *line = [MmLine new];
    line.cacheKey = key; line.sourceLength = text.length;
    line.line = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)attributed);
    double ascent = 0;
    line.width = CTLineGetTypographicBounds(line.line, &ascent, NULL, NULL);
    line.ascent = ascent;
    CFRelease(font);
    [state.lines setObject:line forKey:key cost:text.length * 48 + 64];
    return line;
}

static MmGlyph *mm_glyph(MmState *state, CTFontRef font, CGGlyph glyph, double pointSize) {
    double bucket = fmax(8, fmin(128, ceil(pointSize * state.scale / 4.0) * 4.0));
    CFStringRef postscript = CTFontCopyPostScriptName(font);
    NSString *key = [NSString stringWithFormat:@"%@/%u/%.0f", (__bridge NSString *)postscript, glyph, bucket];
    CFRelease(postscript);
    MmGlyph *cached = state.glyphs[key];
    if (cached) return cached;
    CTFontRef raster = CTFontCreateCopyWithAttributes(font, bucket, NULL, NULL);
    CGRect bounds = CTFontGetBoundingRectsForGlyphs(raster, kCTFontOrientationHorizontal, &glyph, NULL, 1);
    double originX = floor(bounds.origin.x) - 2, originY = floor(bounds.origin.y) - 2;
    NSUInteger width = (NSUInteger)fmax(1, ceil(CGRectGetMaxX(bounds)) - originX + 2);
    NSUInteger height = (NSUInteger)fmax(1, ceil(CGRectGetMaxY(bounds)) - originY + 2);
    if (width > 256 || height > 256) { CFRelease(raster); return nil; }
    if (state.atlasX + width > MmAtlasSize) { state.atlasX = 0; state.atlasY += state.atlasRow; state.atlasRow = 0; }
    if (state.atlasY + height > MmAtlasSize) { state.resetAtlas = YES; CFRelease(raster); return nil; }
    NSMutableData *pixels = [NSMutableData dataWithLength:width * height];
    CGColorSpaceRef gray = CGColorSpaceCreateDeviceGray();
    CGContextRef bitmap = CGBitmapContextCreate(pixels.mutableBytes, width, height, 8, width, gray, (CGBitmapInfo)kCGImageAlphaNone);
    CGColorSpaceRelease(gray);
    if (!bitmap) { CFRelease(raster); return nil; }
    CGContextSetGrayFillColor(bitmap, 1, 1);
    CGContextSetShouldAntialias(bitmap, YES);
    CGPoint position = CGPointMake(-originX, -originY);
    CTFontDrawGlyphs(raster, &glyph, &position, 1, bitmap);
    CGContextRelease(bitmap);
    CFRelease(raster);
    for (NSUInteger y = 0; y < height; ++y) memcpy((uint8_t *)state.atlasPixels.mutableBytes + (state.atlasY + y) * MmAtlasSize + state.atlasX, (uint8_t *)pixels.bytes + y * width, width);
    state.dirtyMinY = MIN(state.dirtyMinY, state.atlasY);
    state.dirtyMaxY = MAX(state.dirtyMaxY, state.atlasY + height);
    MmGlyph *entry = [MmGlyph new];
    entry.x = state.atlasX; entry.y = state.atlasY; entry.width = width; entry.height = height;
    entry.originX = originX; entry.originY = originY; entry.rasterSize = bucket;
    state.glyphs[key] = entry;
    state.atlasX += width; state.atlasRow = MAX(state.atlasRow, height); state.atlasUsed += width * height;
    MmFrameStats stats = state.stats; stats.atlas_upload_bytes += width * height; state.stats = stats;
    return entry;
}

MmContext *mm_open(const char *title, double width, double height, int hidden) {
    @autoreleasepool {
        if (![NSThread isMainThread]) { MmStartupError = @"Window must be created on the main thread"; return NULL; }
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:hidden ? NSApplicationActivationPolicyAccessory : NSApplicationActivationPolicyRegular];
        MmState *state = [MmState new]; state.hidden = hidden != 0; state.error = @"";
        state.events = [NSMutableArray new]; state.instances = [NSMutableData dataWithCapacity:1024 * sizeof(MmQuad)];
        state.glyphs = [NSMutableDictionary new]; state.lines = [NSCache new]; state.lines.countLimit = 8192; state.lines.totalCostLimit = 4 * 1024 * 1024;
        state.device = MTLCreateSystemDefaultDevice();
        if (!state.device) { MmStartupError = @"Metal GPU unavailable"; return NULL; }
        state.queue = [state.device newCommandQueue];
        NSError *error = nil;
        id<MTLLibrary> library = [state.device newLibraryWithSource:[NSString stringWithUTF8String:MmShader] options:nil error:&error];
        if (!library) { MmStartupError = error.localizedDescription ?: @"Metal shader compilation failed"; return NULL; }
        MTLRenderPipelineDescriptor *pipeline = [MTLRenderPipelineDescriptor new];
        pipeline.vertexFunction = [library newFunctionWithName:@"mm_vertex"];
        pipeline.fragmentFunction = [library newFunctionWithName:@"mm_fragment"];
        pipeline.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;
        pipeline.colorAttachments[0].blendingEnabled = YES;
        pipeline.colorAttachments[0].sourceRGBBlendFactor = MTLBlendFactorOne;
        pipeline.colorAttachments[0].destinationRGBBlendFactor = MTLBlendFactorOneMinusSourceAlpha;
        pipeline.colorAttachments[0].sourceAlphaBlendFactor = MTLBlendFactorOne;
        pipeline.colorAttachments[0].destinationAlphaBlendFactor = MTLBlendFactorOneMinusSourceAlpha;
        state.pipeline = [state.device newRenderPipelineStateWithDescriptor:pipeline error:&error];
        if (!state.pipeline) { MmStartupError = error.localizedDescription ?: @"Metal pipeline creation failed"; return NULL; }
        MTLTextureDescriptor *atlas = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Unorm width:MmAtlasSize height:MmAtlasSize mipmapped:NO];
        atlas.usage = MTLTextureUsageShaderRead; atlas.storageMode = MTLStorageModePrivate;
        state.atlas = [state.device newTextureWithDescriptor:atlas];
        if (!state.atlas) { MmStartupError = @"Could not allocate the 16 MiB glyph atlas"; return NULL; }
        state.atlasPixels = [NSMutableData dataWithLength:MmAtlasSize * MmAtlasSize]; state.dirtyMinY = NSNotFound; state.generation = 1;
        state.slots = @[[MmSlot new], [MmSlot new], [MmSlot new]];
        NSRect frame = NSMakeRect(0, 0, fmax(400, fmin(8192, width)), fmax(300, fmin(8192, height)));
        state.window = [[NSWindow alloc] initWithContentRect:frame styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
        state.window.title = title ? [NSString stringWithUTF8String:title] : @"megamap";
        state.window.contentMinSize = NSMakeSize(400,300); state.window.contentMaxSize = NSMakeSize(8192,8192);
        state.window.releasedWhenClosed = NO; state.window.acceptsMouseMovedEvents = YES; state.window.delegate = state;
        state.view = [[MmView alloc] initWithFrame:frame]; state.view.state = state; state.view.input = @""; state.view.marked = @"";
        state.view.selection = NSMakeRange(0, 0); state.view.wantsLayer = YES;
        state.layer = [CAMetalLayer layer]; state.layer.device = state.device; state.layer.pixelFormat = MTLPixelFormatBGRA8Unorm;
        state.layer.framebufferOnly = NO; state.layer.allowsNextDrawableTimeout = YES; state.layer.displaySyncEnabled = YES;
        state.view.layer = state.layer; state.window.contentView = state.view;
        state.view.accessibilityRole = NSAccessibilityGroupRole; state.view.accessibilityLabel = @"megamap";
        state.appDelegate = [MmAppDelegate new]; state.appDelegate.state = state; NSApp.delegate = state.appDelegate;
        NSMenu *menu = [NSMenu new]; NSMenuItem *app = [NSMenuItem new]; [menu addItem:app]; NSMenu *commands = [NSMenu new];
        [commands addItemWithTitle:@"Quit megamap" action:@selector(terminate:) keyEquivalent:@"q"]; app.submenu = commands; NSApp.mainMenu = menu;
        [NSApp finishLaunching]; [state.window center]; [state.window makeFirstResponder:state.view];
        if (!hidden) [state.window orderFront:nil]; else [state.window orderOut:nil];
        [state sizeChanged];
        return (__bridge_retained void *)state;
    }
}

static NSData *mm_cstring(NSString *text) {
    NSMutableData *bytes = [[text ?: @"" dataUsingEncoding:NSUTF8StringEncoding] mutableCopy];
    uint8_t zero = 0; [bytes appendBytes:&zero length:1]; return bytes;
}

const char *mm_last_error(MmContext *context) {
    @autoreleasepool {
        if (context) { MmState *state = mm_state(context); state.borrowedBytes = mm_cstring(state.error); return state.borrowedBytes.bytes; }
        MmStartupErrorBytes = mm_cstring(MmStartupError); return MmStartupErrorBytes.bytes;
    }
}

int mm_poll(MmContext *context, MmEvent *output, double wait_ms) {
    @try { @autoreleasepool {
        MmState *state = mm_state(context);
        if (!state || state.closed || !output) return 0;
        if (state.inputOverflow) {
            *output = (MmEvent){0}; output->kind = 11;
            state.borrowedBytes = [state.error dataUsingEncoding:NSUTF8StringEncoding];
            output->text = state.borrowedBytes.bytes;
            output->text_len = state.borrowedBytes.length;
            state.inputOverflow = NO; return 1;
        }
        if (!state.events.count) {
            NSDate *until = [NSDate dateWithTimeIntervalSinceNow:fmax(0, fmin(50, wait_ms)) / 1000.0];
            NSEvent *event = [NSApp nextEventMatchingMask:NSEventMaskAny untilDate:until inMode:NSDefaultRunLoopMode dequeue:YES];
            if (event) [NSApp sendEvent:event];
            [NSApp updateWindows];
        }
        if (!state.events.count) return 0;
        state.lastEvent = state.events.firstObject; [state.events removeObjectAtIndex:0];
        *output = state.lastEvent.event;
        output->text = state.lastEvent.bytes.bytes;
        output->text_len = state.lastEvent.bytes.length;
        return 1;
    }
    } @catch(NSException *exception) {
        fprintf(stderr, "megamap native poll: %s: %s\n", exception.name.UTF8String, exception.reason.UTF8String);
        MmState *state = mm_state(context); state.error = exception.reason ?: @"Native event error";
        state.borrowedBytes = [state.error dataUsingEncoding:NSUTF8StringEncoding];
        *output = (MmEvent){0}; output->kind = 11; output->text = state.borrowedBytes.bytes; output->text_len = state.borrowedBytes.length; return 1;
    }
}

void mm_size(MmContext *context, double *width, double *height, double *scale) {
    MmState *state = mm_state(context); if (width) *width = state.width; if (height) *height = state.height; if (scale) *scale = state.scale;
}

void mm_begin(MmContext *context, const float clear[4]) {
    @autoreleasepool {
    MmState *state = mm_state(context); state.started = CACurrentMediaTime(); state.instances.length = 0;
    state.stats = (MmFrameStats){0}; state.clearColor = MTLClearColorMake(clear[0], clear[1], clear[2], clear[3]);
    if (state.resetAtlas) {
        BOOL busy = NO;
        for (MmSlot *slot in state.slots) if (slot.command && slot.command.status < MTLCommandBufferStatusCompleted) busy = YES;
        if (!busy) { [state.glyphs removeAllObjects]; state.atlasX = 0; state.atlasY = 0; state.atlasRow = 0; state.atlasUsed = 0; state.resetAtlas = NO; state.dirtyMinY = NSNotFound; state.dirtyMaxY = 0; state.generation++; }
    }
    }
}

void mm_quads(MmContext *context, const MmQuad *quads, size_t count) {
    @autoreleasepool {
    MmState *state = mm_state(context); if (!quads) return;
    NSUInteger existing = state.instances.length / sizeof(MmQuad);
    size_t available = MIN(count, (size_t)(MmMaxInstances - existing));
    NSUInteger offset = state.instances.length;
    state.instances.length = offset + available * sizeof(MmQuad);
    MmQuad *destination = (MmQuad *)((uint8_t *)state.instances.mutableBytes + offset);
    size_t written = 0;
    for (size_t i = 0; i < available; ++i) {
        BOOL valid = YES; for (NSUInteger j = 0; j < 4; ++j) if (!isfinite(quads[i].rect[j]) || !isfinite(quads[i].clip[j])) valid = NO;
        if (valid && quads[i].rect[2] > 0 && quads[i].rect[3] > 0) destination[written++] = quads[i];
    }
    state.instances.length = offset + written * sizeof(MmQuad);
    MmFrameStats stats = state.stats; stats.dropped_instances += count - written; state.stats = stats;
    }
}

double mm_text_width(MmContext *context, const uint8_t *bytes, size_t length, double size, int mono) {
    @autoreleasepool {
    if (!isfinite(size) || size <= 0) return 0;
    return mm_line(mm_state(context), mm_string(bytes, length), size, mono != 0).width;
    }
}

double mm_text(MmContext *context, const uint8_t *bytes, size_t length, double x, double y, double size, const float color[4], const float clip[4], int mono) {
    @autoreleasepool {
    if (!isfinite(x) || !isfinite(y) || !isfinite(size) || size <= 0 || !bytes) return 0;
    MmState *state = mm_state(context); MmLine *line = mm_line(state, mm_string(bytes, length), size, mono != 0);
    if (!line.renderQuads || line.generation != state.generation || line.rasterScale != state.scale || line.missingGlyphs) {
        line.renderQuads = [NSMutableData new]; line.missingGlyphs = 0;
        CFArrayRef runs = CTLineGetGlyphRuns(line.line);
        for (CFIndex i = 0; i < CFArrayGetCount(runs); ++i) {
            CTRunRef run = (CTRunRef)CFArrayGetValueAtIndex(runs, i); CFIndex count = CTRunGetGlyphCount(run);
            CTFontRef font = (CTFontRef)CFDictionaryGetValue(CTRunGetAttributes(run), kCTFontAttributeName);
            double pointSize = CTFontGetSize(font);
            for (CFIndex j = 0; j < count; ++j) {
                CGGlyph glyph; CGPoint position; CTRunGetGlyphs(run, CFRangeMake(j, 1), &glyph); CTRunGetPositions(run, CFRangeMake(j, 1), &position);
                if (!glyph) continue;
                MmGlyph *entry = mm_glyph(state, font, glyph, pointSize);
                if (!entry) { line.missingGlyphs++; continue; }
                double factor = pointSize / entry.rasterSize;
                MmQuad quad = {0};
                quad.rect[0] = (float)(position.x + entry.originX * factor);
                quad.rect[1] = (float)(line.ascent - position.y - (entry.originY + entry.height) * factor);
                quad.rect[2] = (float)(entry.width * factor); quad.rect[3] = (float)(entry.height * factor);
                quad.uv[0] = (float)entry.x / MmAtlasSize; quad.uv[1] = (float)entry.y / MmAtlasSize;
                quad.uv[2] = (float)entry.width / MmAtlasSize; quad.uv[3] = (float)entry.height / MmAtlasSize;
                quad.params[2] = 1;
                [line.renderQuads appendBytes:&quad length:sizeof(quad)];
            }
        }
        line.generation = state.generation; line.rasterScale = state.scale;
        [state.lines setObject:line forKey:line.cacheKey cost:line.sourceLength * 48 + line.renderQuads.length + 64];
    }
    NSUInteger existing = state.instances.length / sizeof(MmQuad), count = line.renderQuads.length / sizeof(MmQuad);
    NSUInteger available = MIN(count, MmMaxInstances - existing), offset = state.instances.length;
    state.instances.length = offset + available * sizeof(MmQuad);
    MmQuad *destination = (MmQuad *)((uint8_t *)state.instances.mutableBytes + offset);
    const MmQuad *source = line.renderQuads.bytes;
    for (NSUInteger i = 0; i < available; ++i) {
        MmQuad quad = source[i]; quad.rect[0] += (float)x; quad.rect[1] += (float)y;
        memcpy(quad.color, color, sizeof(quad.color)); memcpy(quad.clip, clip, sizeof(quad.clip)); destination[i] = quad;
    }
    MmFrameStats stats = state.stats; stats.dropped_instances += count - available; stats.dropped_glyphs += line.missingGlyphs; state.stats = stats;
    return line.width;
    }
}

int mm_present(MmContext *context, MmFrameStats *output) {
    @autoreleasepool {
        MmState *state = mm_state(context); if (!state || state.closed) return 0;
        MmFrameStats stats = state.stats; stats.skipped = 0;
        stats.instances = state.instances.length / sizeof(MmQuad); stats.atlas_glyphs = state.glyphs.count; stats.atlas_used_bytes = state.atlasUsed;
        MmSlot *available = nil;
        for (NSUInteger i = 0; i < state.slots.count; ++i) {
            MmSlot *slot = state.slots[(state.slotIndex + i) % state.slots.count];
            if (slot.command.status == MTLCommandBufferStatusError) { state.error = slot.command.error.localizedDescription ?: @"Metal command failed"; return 0; }
            if (!slot.command || slot.command.status >= MTLCommandBufferStatusCompleted) { available = slot; state.slotIndex = (state.slotIndex + i + 1) % state.slots.count; break; }
        }
        if (!available) { stats.skipped = 1; state.stats = stats; if (output) *output = stats; return 1; }
        if (available.command.status == MTLCommandBufferStatusCompleted) stats.gpu_ms = fmax(0, (available.command.GPUEndTime - available.command.GPUStartTime) * 1000.0);
        NSUInteger width = (NSUInteger)MAX(1, ceil(state.width * state.scale)), height = (NSUInteger)MAX(1, ceil(state.height * state.scale));
        if (width > 16384 || height > 16384 || width * height > 64 * 1024 * 1024) { state.error = @"Window backing texture exceeds the 256 MiB budget"; return 0; }
        if (!state.target || state.target.width != width || state.target.height != height) {
            MTLTextureDescriptor *target = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:width height:height mipmapped:NO];
            target.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead; target.storageMode = MTLStorageModePrivate;
            state.target = [state.device newTextureWithDescriptor:target];
            if (!state.target) { state.error = @"Could not allocate the Metal frame texture"; return 0; }
        }
        NSUInteger bytes = MAX(sizeof(MmQuad), state.instances.length);
        if (!available.buffer || available.capacity < bytes) {
            available.capacity = MAX(bytes, MAX((NSUInteger)4096, available.capacity * 2));
            available.capacity = MIN(available.capacity, MmMaxInstances * sizeof(MmQuad));
            available.buffer = [state.device newBufferWithLength:available.capacity options:MTLResourceStorageModeShared];
            if (!available.buffer) { state.error = @"Could not allocate the Metal instance buffer"; return 0; }
        }
        if (state.instances.length) memcpy(available.buffer.contents, state.instances.bytes, state.instances.length);
        id<MTLCommandBuffer> command = [state.queue commandBuffer];
        if (state.dirtyMinY != NSNotFound) {
            NSUInteger rows = state.dirtyMaxY - state.dirtyMinY, uploadBytes = MmAtlasSize * rows;
            const uint8_t *pixels = (const uint8_t *)state.atlasPixels.bytes + state.dirtyMinY * MmAtlasSize;
            id<MTLBuffer> upload = [state.device newBufferWithBytes:pixels length:uploadBytes options:MTLResourceStorageModeShared];
            if (!upload) { state.error = @"Could not allocate the glyph upload buffer"; return 0; }
            id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
            [blit copyFromBuffer:upload sourceOffset:0 sourceBytesPerRow:MmAtlasSize sourceBytesPerImage:uploadBytes sourceSize:MTLSizeMake(MmAtlasSize, rows, 1) toTexture:state.atlas destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0,state.dirtyMinY,0)];
            [blit endEncoding]; stats.atlas_upload_bytes = uploadBytes;
        } else stats.atlas_upload_bytes = 0;
        MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
        pass.colorAttachments[0].texture = state.target; pass.colorAttachments[0].loadAction = MTLLoadActionClear;
        pass.colorAttachments[0].storeAction = MTLStoreActionStore; pass.colorAttachments[0].clearColor = state.clearColor;
        id<MTLRenderCommandEncoder> encoder = [command renderCommandEncoderWithDescriptor:pass];
        [encoder setRenderPipelineState:state.pipeline]; [encoder setVertexBuffer:available.buffer offset:0 atIndex:0];
        float view[2] = {(float)state.width, (float)state.height}; [encoder setVertexBytes:view length:sizeof(view) atIndex:1];
        [encoder setFragmentBuffer:available.buffer offset:0 atIndex:0]; [encoder setFragmentTexture:state.atlas atIndex:0];
        if (stats.instances) { [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:6 instanceCount:stats.instances]; stats.draw_calls = 1; }
        [encoder endEncoding];
        if (!state.hidden && state.window.isVisible) {
            id<CAMetalDrawable> drawable = [state.layer nextDrawable];
            if (drawable) {
                id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
                [blit copyFromTexture:state.target sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0,0,0) sourceSize:MTLSizeMake(MIN(width,drawable.texture.width),MIN(height,drawable.texture.height),1) toTexture:drawable.texture destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0,0,0)];
                [blit endEncoding]; [command presentDrawable:drawable]; stats.blits = 1;
            }
        }
        [command commit]; available.command = command;
        state.dirtyMinY = NSNotFound; state.dirtyMaxY = 0;
        stats.cpu_ms = (CACurrentMediaTime() - state.started) * 1000.0; state.stats = stats;
        if (output) *output = stats; return 1;
    }
}

int mm_capture(MmContext *context, const char *png_path) {
    @autoreleasepool {
        MmState *state = mm_state(context);
        if (state.stats.skipped) {
            for (MmSlot *slot in state.slots) if (slot.command) { [slot.command waitUntilCompleted]; break; }
            if (!mm_present(context, NULL)) return 0;
        }
        if (!state.target || !png_path) { state.error = @"No frame is available to capture"; return 0; }
        NSUInteger width = state.target.width, height = state.target.height, stride = (width * 4 + 255) & ~(NSUInteger)255;
        id<MTLBuffer> readback = [state.device newBufferWithLength:stride * height options:MTLResourceStorageModeShared];
        if (!readback) { state.error = @"Could not allocate capture readback"; return 0; }
        id<MTLCommandBuffer> command = [state.queue commandBuffer]; id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
        [blit copyFromTexture:state.target sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0,0,0) sourceSize:MTLSizeMake(width,height,1) toBuffer:readback destinationOffset:0 destinationBytesPerRow:stride destinationBytesPerImage:stride * height];
        [blit endEncoding]; [command commit]; [command waitUntilCompleted];
        if (command.status == MTLCommandBufferStatusError) { state.error = command.error.localizedDescription ?: @"GPU capture failed"; return 0; }
        CGColorSpaceRef color = CGColorSpaceCreateDeviceRGB();
        CGContextRef bitmap = CGBitmapContextCreate(readback.contents, width, height, 8, stride, color, kCGBitmapByteOrder32Little | kCGImageAlphaPremultipliedFirst);
        CGColorSpaceRelease(color);
        if (!bitmap) { state.error = @"Could not create capture bitmap"; return 0; }
        CGImageRef image = CGBitmapContextCreateImage(bitmap); CGContextRelease(bitmap);
        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:png_path]];
        CGImageDestinationRef destination = CGImageDestinationCreateWithURL((__bridge CFURLRef)url, CFSTR("public.png"), 1, NULL);
        BOOL ok = destination && image;
        if (ok) { CGImageDestinationAddImage(destination, image, NULL); ok = CGImageDestinationFinalize(destination); }
        if (destination) CFRelease(destination); if (image) CGImageRelease(image);
        if (!ok) state.error = @"Could not write capture PNG";
        return ok ? 1 : 0;
    }
}

void mm_text_state(MmContext *context, const uint8_t *bytes, size_t length, uint64_t start, uint64_t selection_length, const double caret[4]) {
    @autoreleasepool {
    MmState *state = mm_state(context); NSString *text = mm_string(bytes, length); state.view.input = text;
    start = MIN(start, text.length); selection_length = MIN(selection_length, text.length - start);
    state.view.selection = NSMakeRange((NSUInteger)start, (NSUInteger)selection_length);
    state.view.caret = NSMakeRect(caret[0], caret[1], caret[2], caret[3]);
    }
}

void mm_cursor(MmContext *context, uint32_t cursor) {
    @autoreleasepool {
    (void)context;
    switch (cursor) { case 1: [[NSCursor pointingHandCursor] set]; break; case 2: [[NSCursor openHandCursor] set]; break;
        case 3: [[NSCursor closedHandCursor] set]; break; case 4: [[NSCursor IBeamCursor] set]; break; default: [[NSCursor arrowCursor] set]; }
    }
}
void mm_cancel_composition(MmContext *context) {
    @autoreleasepool {
    MmState *state = mm_state(context); state.view.marked = @""; [state.view.inputContext discardMarkedText];
    }
}
void mm_title(MmContext *context, const char *title) { @autoreleasepool { if (title) mm_state(context).window.title = [NSString stringWithUTF8String:title]; } }
const char *mm_clipboard(MmContext *context) {
    @autoreleasepool {
    MmState *state = mm_state(context); state.clipboard = [NSPasteboard.generalPasteboard stringForType:NSPasteboardTypeString] ?: @"";
    state.borrowedBytes = mm_cstring(state.clipboard); return state.borrowedBytes.bytes;
    }
}
void mm_set_clipboard(MmContext *context, const char *text) {
    @autoreleasepool {
    (void)context; [NSPasteboard.generalPasteboard clearContents];
    [NSPasteboard.generalPasteboard setString:text ? [NSString stringWithUTF8String:text] : @"" forType:NSPasteboardTypeString];
    }
}

void mm_accessibility(MmContext *context, const MmAccessibility *elements, size_t count) {
    @autoreleasepool {
    MmState *state = mm_state(context); NSMutableArray<MmAXElement *> *children = [NSMutableArray new];
    for (size_t i = 0; i < MIN(count, (size_t)2048); ++i) {
        const MmAccessibility *item = &elements[i];
        NSString *role = item->role == 1 ? NSAccessibilityTextFieldRole : item->role == 2 ? NSAccessibilityCheckBoxRole : item->role == 3 ? NSAccessibilityPopUpButtonRole : NSAccessibilityButtonRole;
        NSRect local = NSMakeRect(item->rect[0],item->rect[1],item->rect[2],item->rect[3]);
        NSRect screen = [state.window convertRectToScreen:[state.view convertRect:local toView:nil]];
        MmAXElement *element = [MmAXElement accessibilityElementWithRole:role frame:screen label:item->label ? [NSString stringWithUTF8String:item->label] : @"" parent:state.view];
        element.state = state; element.controlId = item->id; element.accessibilityIdentifier = [NSString stringWithFormat:@"%llu", (unsigned long long)item->id];
        element.accessibilityEnabled = item->enabled != 0; element.accessibilityFocused = item->focused != 0;
        element.accessibilityValue = item->value ? [NSString stringWithUTF8String:item->value] : @"";
        [children addObject:element];
    }
    state.accessibility = children; state.view.accessibilityChildren = children;
    }
}

void mm_close(MmContext *context) {
    if (!context) return;
    @autoreleasepool {
        MmState *state = CFBridgingRelease(context); state.closed = YES; state.view.state = nil;
        state.window.delegate = nil; [state.window orderOut:nil]; [state.window close];
        if (NSApp.delegate == state.appDelegate) NSApp.delegate = nil;
        state.window = nil; state.view = nil; [NSApp updateWindows];
    }
}

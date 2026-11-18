#import "../megamap.m"
#include <assert.h>

@interface MmInputSpy : MmView
@property NSUInteger interpreted;
@end
@implementation MmInputSpy
- (void)interpretKeyEvents:(NSArray<NSEvent *> *)events { (void)events; self.interpreted++; }
@end

int main(void) {
    @autoreleasepool {
        MmState *state = [MmState new]; state.events = [NSMutableArray new];
        MmInputSpy *view = [MmInputSpy new]; view.state = state; state.view = view; view.input = @"abc"; view.marked = @"";
        NSEvent *left = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:NSEventModifierFlagCommand timestamp:0 windowNumber:0 context:nil characters:@"\uF702" charactersIgnoringModifiers:@"\uF702" isARepeat:NO keyCode:123];
        [view keyDown:left];
        assert(state.events.lastObject.event.code == 0x104);
        printf("Command-left: native key code=%u (KEY_HOME=%u); interpreted=%lu\n", state.events.lastObject.event.code, 0x104, (unsigned long)view.interpreted);
        [state.events removeAllObjects];
        [view setMarkedText:@"日本" selectedRange:NSMakeRange(2,0) replacementRange:NSMakeRange(NSNotFound,0)];
        [state.events removeAllObjects];
        NSEvent *enter = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:0 context:nil characters:@"\r" charactersIgnoringModifiers:@"\r" isARepeat:NO keyCode:36];
        [view keyDown:enter];
        assert(view.interpreted == 1 && state.events.count == 0);
        printf("Return during composition: native key code=%u; interpreted=%lu; native hasMarkedText=%d\n", state.events.lastObject.event.code, (unsigned long)view.interpreted, view.hasMarkedText);
        for (NSNumber *type in @[@(NSEventTypeMouseEntered), @(NSEventTypeMouseExited)]) {
            NSEvent *event = [NSEvent enterExitEventWithType:type.unsignedIntegerValue location:NSMakePoint(10,10) modifierFlags:0 timestamp:0 windowNumber:0 context:nil eventNumber:1 trackingNumber:1 userData:NULL];
            [view pointer:event kind:1];
            assert(state.events.lastObject.event.kind == 1);
            assert(state.events.lastObject.event.clicks == 0);
        }
        mm_cancel_composition((__bridge MmContext *)state);
        assert(!view.hasMarkedText);
    }
}

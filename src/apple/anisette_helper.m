#import <Foundation/Foundation.h>
#import <objc/message.h>

static void fail(NSString *message) {
    fprintf(stderr, "%s\n", message.UTF8String);
    exit(1);
}

int main(void) {
    @autoreleasepool {
        NSBundle *bundle = [NSBundle bundleWithPath:@"/System/Library/PrivateFrameworks/AOSKit.framework"];
        if (bundle == nil || ![bundle load]) {
            fail(@"missing AOSKit bundle");
        }

        Class util = NSClassFromString(@"AOSUtilities");
        if (util == Nil) {
            fail(@"missing AOSUtilities");
        }

        SEL selector = NSSelectorFromString(@"retrieveOTPHeadersForDSID:");
        if (![util respondsToSelector:selector]) {
            fail(@"AOSUtilities is missing retrieveOTPHeadersForDSID:");
        }

        NSDictionary *headers = ((id (*)(id, SEL, id))objc_msgSend)(util, selector, @"-2");
        if (![headers isKindOfClass:[NSDictionary class]]) {
            fail(@"retrieveOTPHeadersForDSID returned nil");
        }

        NSString *md = [[headers objectForKey:@"X-Apple-MD"] description];
        NSString *mdm = [[headers objectForKey:@"X-Apple-MD-M"] description];
        if (md.length == 0 || mdm.length == 0) {
            fail(@"anisette headers are missing X-Apple-MD or X-Apple-MD-M");
        }

        NSError *error = nil;
        NSData *json = [NSJSONSerialization dataWithJSONObject:@{
            @"md": md,
            @"md_m": mdm
        } options:0 error:&error];
        if (json == nil) {
            fail([NSString stringWithFormat:@"failed to encode anisette JSON: %@", error]);
        }

        fwrite(json.bytes, 1, json.length, stdout);
    }
    return 0;
}

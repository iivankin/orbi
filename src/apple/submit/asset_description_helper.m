#import <Foundation/Foundation.h>
#import <objc/runtime.h>

static id invoke_id(id target, SEL selector, NSArray *arguments) {
    NSMethodSignature *signature = [target methodSignatureForSelector:selector];
    if (signature == nil) {
        @throw [NSException exceptionWithName:@"MissingSelector"
                                       reason:[NSString stringWithFormat:@"%@ does not respond to %@",
                                               target,
                                               NSStringFromSelector(selector)]
                                     userInfo:nil];
    }

    NSInvocation *invocation = [NSInvocation invocationWithMethodSignature:signature];
    invocation.target = target;
    invocation.selector = selector;
    NSUInteger index = 2;
    for (id argument in arguments) {
        id value = argument == [NSNull null] ? nil : argument;
        [invocation setArgument:&value atIndex:index];
        index += 1;
    }
    [invocation invoke];

    const char *return_type = signature.methodReturnType;
    if (strcmp(return_type, @encode(void)) == 0) {
        return nil;
    }

    __unsafe_unretained id result = nil;
    [invocation getReturnValue:&result];
    return result;
}

static id invoke_id_with_string_bool_and_id(id target,
                                            SEL selector,
                                            NSString *string_value,
                                            BOOL bool_value,
                                            id object_value) {
    NSMethodSignature *signature = [target methodSignatureForSelector:selector];
    if (signature == nil) {
        @throw [NSException exceptionWithName:@"MissingSelector"
                                       reason:[NSString stringWithFormat:@"%@ does not respond to %@",
                                               target,
                                               NSStringFromSelector(selector)]
                                     userInfo:nil];
    }

    NSInvocation *invocation = [NSInvocation invocationWithMethodSignature:signature];
    invocation.target = target;
    invocation.selector = selector;
    id object_argument = object_value == [NSNull null] ? nil : object_value;
    [invocation setArgument:&string_value atIndex:2];
    [invocation setArgument:&bool_value atIndex:3];
    [invocation setArgument:&object_argument atIndex:4];
    [invocation invoke];

    __unsafe_unretained id result = nil;
    [invocation getReturnValue:&result];
    return result;
}

static BOOL invoke_bool_with_file_and_platform(id target,
                                               SEL selector,
                                               NSString *file_path,
                                               NSString *platform_name,
                                               id auth_context,
                                               NSString **first_out,
                                               NSString **second_out) {
    NSMethodSignature *signature = [target methodSignatureForSelector:selector];
    if (signature == nil) {
        @throw [NSException exceptionWithName:@"MissingSelector"
                                       reason:[NSString stringWithFormat:@"%@ does not respond to %@",
                                               target,
                                               NSStringFromSelector(selector)]
                                     userInfo:nil];
    }

    NSInvocation *invocation = [NSInvocation invocationWithMethodSignature:signature];
    invocation.target = target;
    invocation.selector = selector;
    NSString *first = nil;
    NSString *second = nil;
    id log = nil;
    [invocation setArgument:&file_path atIndex:2];
    [invocation setArgument:&platform_name atIndex:3];
    [invocation setArgument:&first atIndex:4];
    [invocation setArgument:&second atIndex:5];
    [invocation setArgument:&auth_context atIndex:6];
    [invocation setArgument:&log atIndex:7];
    [invocation invoke];

    if (first_out != NULL) {
        *first_out = first;
    }
    if (second_out != NULL) {
        *second_out = second;
    }

    BOOL success = NO;
    [invocation getReturnValue:&success];
    return success;
}

static NSArray<NSString *> *matching_paths_in_directory(NSString *directory,
                                                        NSString *prefix,
                                                        NSString *suffix) {
    NSFileManager *file_manager = [NSFileManager defaultManager];
    NSArray<NSString *> *entries =
        [file_manager contentsOfDirectoryAtPath:directory error:nil] ?: @[];
    NSMutableArray<NSString *> *paths = [NSMutableArray array];
    for (NSString *entry in entries) {
        if (![entry hasPrefix:prefix] || ![entry hasSuffix:suffix]) {
            continue;
        }
        [paths addObject:[directory stringByAppendingPathComponent:entry]];
    }
    return paths;
}

static NSString *newest_added_path(NSArray<NSString *> *before_paths,
                                   NSArray<NSString *> *after_paths) {
    NSMutableSet<NSString *> *known = [NSMutableSet setWithArray:before_paths];
    NSString *selected = nil;
    NSDate *selected_date = nil;
    NSFileManager *file_manager = [NSFileManager defaultManager];
    for (NSString *candidate in after_paths) {
        if ([known containsObject:candidate]) {
            continue;
        }
        NSDictionary<NSFileAttributeKey, id> *attributes =
            [file_manager attributesOfItemAtPath:candidate error:nil];
        NSDate *modified = attributes[NSFileModificationDate];
        if (selected == nil || [selected_date compare:modified] == NSOrderedAscending) {
            selected = candidate;
            selected_date = modified;
        }
    }
    return selected;
}

static NSString *copy_to_directory(NSString *source_path, NSString *output_directory) {
    if (source_path == nil || output_directory == nil) {
        return nil;
    }
    NSFileManager *file_manager = [NSFileManager defaultManager];
    if (![file_manager fileExistsAtPath:source_path]) {
        return nil;
    }
    NSString *destination =
        [output_directory stringByAppendingPathComponent:[source_path lastPathComponent]];
    [file_manager removeItemAtPath:destination error:nil];
    NSError *error = nil;
    if (![file_manager copyItemAtPath:source_path toPath:destination error:&error]) {
        fprintf(stderr,
                "failed to copy %s to %s: %s\n",
                source_path.UTF8String,
                destination.UTF8String,
                error.localizedDescription.UTF8String);
        return nil;
    }
    return destination;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc != 5) {
            fprintf(stderr,
                    "usage: %s <ipa-path> <platform> <provider-public-id> <output-dir>\n",
                    argv[0]);
            return 2;
        }

        NSString *ipa_path = [NSString stringWithUTF8String:argv[1]];
        NSString *platform_name = [NSString stringWithUTF8String:argv[2]];
        NSString *provider_public_id = [NSString stringWithUTF8String:argv[3]];
        NSString *output_directory = [NSString stringWithUTF8String:argv[4]];

        NSBundle *framework_bundle =
            [NSBundle bundleWithPath:@"/Applications/Transporter.app/Contents/Frameworks/ContentDelivery.framework"];
        if (framework_bundle == nil || ![framework_bundle load]) {
            fprintf(stderr, "failed to load ContentDelivery.framework\n");
            return 1;
        }

        Class content_delivery_class = NSClassFromString(@"ContentDelivery");
        if (content_delivery_class != Nil) {
            invoke_id(content_delivery_class, NSSelectorFromString(@"setEnableDebugLogging:"), @[@YES]);
            invoke_id(content_delivery_class, NSSelectorFromString(@"setEnableStderrLogging:"), @[@YES]);
        }

        Class auth_context_class = NSClassFromString(@"CDAuthContext");
        Class swinfo_task_class = NSClassFromString(@"CDSwinfoTask");
        if (auth_context_class == Nil || swinfo_task_class == Nil) {
            fprintf(stderr, "missing ContentDelivery runtime classes\n");
            return 1;
        }

        id auth_context = invoke_id_with_string_bool_and_id(
            [auth_context_class alloc],
            NSSelectorFromString(@"initWithProviderPublicID:canJWTAuthenticate:delegate:"),
            provider_public_id,
            NO,
            nil
        );
        if (auth_context == nil) {
            fprintf(stderr, "failed to create CDAuthContext\n");
            return 1;
        }

        NSString *temp_directory = NSTemporaryDirectory();
        NSArray<NSString *> *before_assets =
            matching_paths_in_directory(temp_directory, @"asset-description-", @".xml");
        NSArray<NSString *> *before_spi =
            matching_paths_in_directory(temp_directory, @"DTAppAnalyzerExtractorOutput-", @".zip");

        NSString *reported_asset = nil;
        NSString *reported_spi = nil;
        BOOL reported_success = invoke_bool_with_file_and_platform(
            [[swinfo_task_class alloc] init],
            NSSelectorFromString(@"describeFile:platformStr:outDescriptionFile:outSPIFile:authContext:log:"),
            ipa_path,
            platform_name,
            auth_context,
            &reported_asset,
            &reported_spi
        );

        NSArray<NSString *> *after_assets =
            matching_paths_in_directory(temp_directory, @"asset-description-", @".xml");
        NSArray<NSString *> *after_spi =
            matching_paths_in_directory(temp_directory, @"DTAppAnalyzerExtractorOutput-", @".zip");

        NSString *asset_source = reported_asset;
        if (asset_source == nil || ![[NSFileManager defaultManager] fileExistsAtPath:asset_source]) {
            asset_source = newest_added_path(before_assets, after_assets);
        }

        NSString *spi_source = reported_spi;
        if (spi_source == nil || ![[NSFileManager defaultManager] fileExistsAtPath:spi_source]) {
            spi_source = newest_added_path(before_spi, after_spi);
        }

        NSString *asset_destination = copy_to_directory(asset_source, output_directory);
        NSString *spi_destination = copy_to_directory(spi_source, output_directory);

        NSDictionary *result = @{
            @"reportedSuccess": @(reported_success),
            @"assetDescriptionPath": asset_destination ?: @"",
            @"spiPath": spi_destination ?: @"",
        };
        NSData *json = [NSJSONSerialization dataWithJSONObject:result options:0 error:nil];
        fwrite(json.bytes, json.length, 1, stdout);
        fputc('\n', stdout);

        return asset_destination != nil ? 0 : 1;
    }
}

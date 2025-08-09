use relay_builder::RelayConfig;
use nostr_sdk::Keys;

fn main() {
    let keys = Keys::generate();
    
    // Test with default values
    let config = RelayConfig::new("ws://localhost:8080", "./test.db", keys.clone());
    println\!("Default config:");
    println\!("  max_subscriptions: {}", config.max_subscriptions);
    println\!("  max_limit: {}", config.max_limit);
    println\!("  channel_size: {}", config.calculate_channel_size());
    
    // Test with custom values
    let config = config.with_subscription_limits(100, 1000);
    println\!("\nCustom config (100 subs, 1000 limit):");
    println\!("  channel_size: {}", config.calculate_channel_size());
    
    // Test with small values
    let config = config.with_subscription_limits(10, 100);
    println\!("\nSmall config (10 subs, 100 limit):");
    println\!("  channel_size: {}", config.calculate_channel_size());
}

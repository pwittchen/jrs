package com.example.bookmarks;

import java.util.List;
import org.springframework.boot.context.properties.ConfigurationProperties;

/**
 * The `bookmarks.*` keys of {@code application.properties}, bound to a record.
 *
 * <p>Constructor binding matches property names to parameter names, which only
 * works because {@code javac} was given {@code -parameters}.
 */
@ConfigurationProperties("bookmarks")
public record BookmarkProperties(int maxSize, List<Seed> seed) {

    public BookmarkProperties {
        seed = seed == null ? List.of() : List.copyOf(seed);
    }

    /** A bookmark to store at startup, before anything has been posted. */
    public record Seed(String url, String title) {}
}

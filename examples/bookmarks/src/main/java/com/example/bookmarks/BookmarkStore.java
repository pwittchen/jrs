package com.example.bookmarks;

import java.util.Comparator;
import java.util.List;
import java.util.Optional;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentMap;
import java.util.concurrent.atomic.AtomicLong;
import org.springframework.stereotype.Service;

/**
 * The bookmarks, in memory: a map, a counter and a size limit. It is a Spring
 * bean, but nothing in it is Spring, so the unit test just calls {@code new}.
 */
@Service
public class BookmarkStore {

    private final ConcurrentMap<Long, Bookmark> bookmarks = new ConcurrentHashMap<>();
    private final AtomicLong nextId = new AtomicLong(1);
    private final int maxSize;

    public BookmarkStore(BookmarkProperties properties) {
        this.maxSize = properties.maxSize();
        for (BookmarkProperties.Seed seed : properties.seed()) {
            add(seed.url(), seed.title());
        }
    }

    public List<Bookmark> all() {
        return bookmarks.values().stream().sorted(Comparator.comparingLong(Bookmark::id)).toList();
    }

    public Optional<Bookmark> find(long id) {
        return Optional.ofNullable(bookmarks.get(id));
    }

    /** Stores a bookmark, or nothing at all once the store is full. */
    public Optional<Bookmark> add(String url, String title) {
        if (bookmarks.size() >= maxSize) {
            return Optional.empty();
        }
        Bookmark bookmark = new Bookmark(nextId.getAndIncrement(), url, title);
        bookmarks.put(bookmark.id(), bookmark);
        return Optional.of(bookmark);
    }

    public boolean remove(long id) {
        return bookmarks.remove(id) != null;
    }
}

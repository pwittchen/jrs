package com.example.bookmarks;

/** One stored bookmark. Jackson serialises the record's components as they are. */
public record Bookmark(long id, String url, String title) {}

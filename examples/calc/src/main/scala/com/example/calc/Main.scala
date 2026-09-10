package com.example.calc

object Main:
  private val samples = List("1 + 2 * 3", "(1 + 2) * 3", "-(2.5 - 4) / 0.5", "10 / 4", "1 / 0")

  def main(args: Array[String]): Unit =
    val inputs = if args.isEmpty then samples else args.toList
    for input <- inputs do println(s"$input = ${Format.evaluate(input)}")

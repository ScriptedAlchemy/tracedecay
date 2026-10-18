def normalize(text):
    return text.strip().lower()


def clamp(value, low, high):
    return max(low, min(value, high))

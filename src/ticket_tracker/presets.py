"""内置常用影院预设。"""

PRESETS = {
    "前滩太古里": {
        "id": "37534",
        "name": "MOViE MOViE 影城（前滩太古里店）",
        "city": "上海",
        "note": "艺术影院，挂幕类活动常有",
    },
    "上海大光明": {
        "id": "2127",
        "name": "大光明影院（南京西路）",
        "city": "上海",
        "note": "首映礼常驻地",
    },
    "上海影城": {
        "id": "2120",
        "name": "上海影城（新华路）",
        "city": "上海",
        "note": "上海电影节主场地",
    },
    "北京万达CBD": {
        "id": "7579",
        "name": "万达影城（北京CBD店）",
        "city": "北京",
        "note": "热门商圈",
    },
    "深圳万象天地": {
        "id": "11823",
        "name": "万象影城（深圳万象天地店）",
        "city": "深圳",
        "note": "热门商圈",
    },
}


def list_presets():
    return PRESETS


def get_preset(name):
    return PRESETS.get(name)
